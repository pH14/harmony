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
COPY workloads/harness/targets/summitdb/go.mod workloads/harness/targets/summitdb/go.sum targets/summitdb/
RUN (cd kit && go mod download) && (cd targets/summitdb && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/summitdb/main.go targets/summitdb/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/summitdb && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/summitdb-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && ! grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/summitdb-harness \
    && test -x /out/summitdb-harness

FROM ${GO_IMAGE} AS instrumented-build

ARG TARGETARCH
ARG SUMMITDB_COMMIT=bc639291bb251f21b05c7e87526faa28f5f8ae23
ARG ANTITHESIS_SDK_VERSION=0.8.0
ARG SUMMITDB_INSTRUMENT=github.com/tidwall/summitdb
ENV CGO_ENABLED=1 GOFLAGS=-trimpath GOTOOLCHAIN=local

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates git gcc libc6-dev make patch \
    && rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
        amd64|arm64) ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

# SummitDB has no go.mod and vendors its dependencies, so it builds in GOPATH
# mode, where vendored packages carry the github.com/tidwall/summitdb prefix.
WORKDIR /gopath/src/github.com/tidwall
RUN git clone --quiet https://github.com/tidwall/summitdb.git \
    && git -C summitdb checkout --quiet "${SUMMITDB_COMMIT}" \
    && test "$(git -C summitdb rev-parse HEAD)" = "${SUMMITDB_COMMIT}" \
    && rm -rf summitdb/.git

RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}"

WORKDIR /build
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

# The wrapper builds the SDK package with the go command, which needs module
# mode even though SummitDB itself builds in GOPATH mode.
RUN printf '#!/bin/sh\nGOPATH=%s GO111MODULE=on exec %s "$@"\n' "$(go env GOPATH)" "$(command -v antithesis-go-toolexec)" \
       > /usr/local/bin/toolexec-modules \
    && chmod 0755 /usr/local/bin/toolexec-modules

RUN mkdir -p /out/summitdb /out/symbols /out/stdlib \
    && sdk_dir=/build/antithesis-sdk-go \
    && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
       bash /build/configure-stdlib.sh "${sdk_dir}" "${SUMMITDB_INSTRUMENT}" /out/stdlib > /out/stdlib/instrumentation.env \
    && source /out/stdlib/instrumentation.env \
    && (cd /gopath/src/github.com/tidwall/summitdb/cmd/summitdb-server \
        && GOPATH=/gopath GO111MODULE=off CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           ANTITHESIS_SDK_MODULE_DIR="${sdk_dir}" \
           ANTITHESIS_SYMBOLS_DIR=/out/symbols \
           ANTITHESIS_SYMBOL_PREFIX=summitdb \
           go build -toolexec=toolexec-modules -o /out/summitdb/summitdb-server .) \
    && test -x /out/summitdb/summitdb-server \
    && set -- /out/symbols/*.sym.tsv \
    && test -s "$1" \
    && grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/summitdb/summitdb-server \
    && sha256sum /out/summitdb/summitdb-server \
       | awk '{print $1 "  /opt/summitdb/summitdb-server"}' > /out/harmony-instrumented-events \
    && cp /gopath/src/github.com/tidwall/summitdb/LICENSE /out/SUMMITDB-LICENSE \
    && cp "${sdk_dir}/LICENSE" /out/ANTITHESIS-SDK-GO-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG SUMMITDB_COMMIT=bc639291bb251f21b05c7e87526faa28f5f8ae23
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

COPY --from=instrumented-build /out/summitdb/summitdb-server /opt/summitdb/summitdb-server
COPY --from=harness-build /out/summitdb-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/*.sym.tsv /symbols/
COPY --from=instrumented-build /out/stdlib /symbols/stdlib
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/SUMMITDB-LICENSE /licenses/summitdb.txt
COPY --from=instrumented-build /out/ANTITHESIS-SDK-GO-LICENSE /licenses/antithesis-sdk-go.txt

RUN chmod 0755 /opt/summitdb/summitdb-server /opt/harmony/summitdb-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -x /opt/summitdb/summitdb-server \
    && test -x /opt/harmony/summitdb-harness \
    && set -- /symbols/*.sym.tsv \
    && test -s "$1" \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/summitdb.txt \
    && test -s /licenses/antithesis-sdk-go.txt

LABEL org.opencontainers.image.title="harmony SummitDB harness workload"
LABEL org.opencontainers.image.version="${SUMMITDB_COMMIT}"
LABEL org.opencontainers.image.description="upstream SummitDB source built through the Antithesis Go instrumentation pipeline, driven by the Harmony harness kit"

ARG HARNESS_BUNDLE=bundle
COPY workloads/harness/targets/summitdb/${HARNESS_BUNDLE} /etc/harmony/bundle
COPY workloads/harness/targets/summitdb/setup.sh workloads/harness/targets/summitdb/node.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1"]
