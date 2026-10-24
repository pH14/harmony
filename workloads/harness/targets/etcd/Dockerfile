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
COPY workloads/harness/targets/etcd/go.mod workloads/harness/targets/etcd/go.sum targets/etcd/
RUN (cd kit && go mod download) && (cd targets/etcd && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/etcd/main.go targets/etcd/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/etcd && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/etcd-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && ! grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/etcd-harness \
    && test -x /out/etcd-harness

FROM ${GO_IMAGE} AS instrumented-build

ARG TARGETARCH
ARG ETCD_VERSION=v3.5.4
ARG ETCD_SOURCE_SHA256=
ARG ANTITHESIS_SDK_VERSION=0.8.0
ARG ETCD_SOURCE_BASE=https://github.com/etcd-io/etcd/archive/refs/tags
ARG ETCD_INSTRUMENT=go.etcd.io/etcd,go.etcd.io/bbolt
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
RUN sha256="${ETCD_SOURCE_SHA256}" \
    && if [ -z "${sha256}" ]; then \
        case "${ETCD_VERSION}" in \
            v3.5.2) sha256=ecb4d2bc76e48ae504d9a00b50bcc906503c4d0324acf9dee79d97d16f7282ad ;; \
            v3.5.3) sha256=f381557feaa42dfe7f40a5c295f95266b7de341f49e76a3119dfaec3d0a24e5e ;; \
            v3.5.4) sha256=10e8c1604578d4a8557263d61d4fbbd2428c8cd8dda3c8a23a8755314af6ef10 ;; \
            v3.5.5) sha256=1a16ab899c2381f3ed68f7c38ab878edd391397f7ff987b22a9cd50aab092b79 ;; \
            v3.5.27) sha256=aa0cd1baf203f6ccb3c56ac4d0c61ed2b0cca4995c2541b884b383d330176ff7 ;; \
            v3.5.28) sha256=c1e873c174c44de5fb148024d3ad741cae11548b5b6d2ac36a003289e55024be ;; \
            *) echo "pass ETCD_SOURCE_SHA256 for ${ETCD_VERSION}" >&2; exit 1 ;; \
        esac; \
    fi \
    && curl --fail --silent --show-error --location --retry 3 \
       --output etcd-source.tar.gz \
       "${ETCD_SOURCE_BASE}/${ETCD_VERSION}.tar.gz" \
    && echo "${sha256}  etcd-source.tar.gz" | sha256sum --check --strict - \
    && mkdir etcd \
    && tar -xf etcd-source.tar.gz -C etcd --strip-components=1 \
    && rm etcd-source.tar.gz \
    && (cd etcd/server && go mod download)

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

RUN mkdir -p /out/etcd /out/symbols /out/stdlib \
    && sdk_dir=/build/antithesis-sdk-go \
    && (cd /build/etcd/server \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           bash /build/configure-stdlib.sh "${sdk_dir}" "${ETCD_INSTRUMENT}" /out/stdlib > /out/stdlib/instrumentation.env \
        && source /out/stdlib/instrumentation.env \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           ANTITHESIS_SDK_MODULE_DIR="${sdk_dir}" \
           ANTITHESIS_SYMBOLS_DIR=/out/symbols \
           ANTITHESIS_SYMBOL_PREFIX=etcd-server \
           go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/etcd/etcd .) \
    && test -x /out/etcd/etcd \
    && set -- /out/symbols/*.sym.tsv \
    && test -s "$1" \
    && grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/etcd/etcd \
    && sha256sum /out/etcd/etcd \
       | awk '{print $1 "  /opt/etcd/etcd"}' > /out/harmony-instrumented-events \
    && cp /build/etcd/LICENSE /out/ETCD-LICENSE \
    && cp "${sdk_dir}/LICENSE" /out/ANTITHESIS-SDK-GO-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG ETCD_VERSION=v3.5.4
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

COPY --from=instrumented-build /out/etcd/etcd /opt/etcd/etcd
COPY --from=harness-build /out/etcd-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/*.sym.tsv /symbols/
COPY --from=instrumented-build /out/stdlib /symbols/stdlib
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/ETCD-LICENSE /licenses/etcd.txt
COPY --from=instrumented-build /out/ANTITHESIS-SDK-GO-LICENSE /licenses/antithesis-sdk-go.txt

RUN chmod 0755 /opt/etcd/etcd /opt/harmony/etcd-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -x /opt/etcd/etcd \
    && test -x /opt/harmony/etcd-harness \
    && set -- /symbols/*.sym.tsv \
    && test -s "$1" \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/etcd.txt \
    && test -s /licenses/antithesis-sdk-go.txt

LABEL org.opencontainers.image.title="harmony etcd harness workload"
LABEL org.opencontainers.image.version="${ETCD_VERSION}"
LABEL org.opencontainers.image.description="upstream etcd source built through the Antithesis Go instrumentation pipeline, driven by the Harmony harness kit"

ARG HARNESS_BUNDLE=bundle
COPY workloads/harness/targets/etcd/${HARNESS_BUNDLE} /etc/harmony/bundle
COPY workloads/harness/targets/etcd/setup.sh workloads/harness/targets/etcd/node.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1", "1"]
