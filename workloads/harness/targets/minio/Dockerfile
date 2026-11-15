# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.27-bookworm@sha256:5cf287a799e6b94384bad13d16b14904c531f51ba65792237e122ce42b392f61
ARG MINIO_GO_IMAGE=golang:1.24-bookworm@sha256:1a6d4452c65dea36aac2e2d606b01b4a029ec90cc1ae53890540ce6173ea77ac

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
COPY workloads/harness/targets/minio/go.mod workloads/harness/targets/minio/go.sum targets/minio/
COPY workloads/harness/targets/minio/cpuid-xgetbv.sh /build/cpuid-xgetbv.sh
RUN (cd kit && go mod download) && (cd targets/minio && go mod download && bash /build/cpuid-xgetbv.sh /build/cpuid)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/minio/main.go targets/minio/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/minio && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/minio-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && ! grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/minio-harness \
    && test -x /out/minio-harness

FROM ${MINIO_GO_IMAGE} AS instrumented-build

ARG TARGETARCH
ARG MINIO_VERSION=RELEASE.2025-10-15T17-29-55Z
ARG MINIO_SOURCE_SHA256=
ARG ANTITHESIS_SDK_VERSION=0.8.0
ARG MINIO_SOURCE_BASE=https://github.com/minio/minio/archive/refs/tags
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
RUN sha256="${MINIO_SOURCE_SHA256}" \
    && if [ -z "${sha256}" ]; then \
        case "${MINIO_VERSION}" in \
            RELEASE.2025-10-15T17-29-55Z) sha256=be6d0bd3696c3a13a35f02d3a0280b64319c67918b4501c5c3d87f96d000085c ;; \
            *) echo "pass MINIO_SOURCE_SHA256 for ${MINIO_VERSION}" >&2; exit 1 ;; \
        esac; \
    fi \
    && curl --fail --silent --show-error --location --retry 3 \
       --output minio-source.tar.gz \
       "${MINIO_SOURCE_BASE}/${MINIO_VERSION}.tar.gz" \
    && echo "${sha256}  minio-source.tar.gz" | sha256sum --check --strict - \
    && mkdir minio \
    && tar -xf minio-source.tar.gz -C minio --strip-components=1 \
    && rm -rf minio-source.tar.gz minio/docs minio/helm minio/helm-releases \
    && (cd minio && go mod download)

COPY workloads/harness/targets/minio/cpuid-xgetbv.sh /build/cpuid-xgetbv.sh
RUN cd /build/minio && bash /build/cpuid-xgetbv.sh /build/cpuid

RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}"

COPY workloads/bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch \
     /build/antithesis-sdk-go-v0.8.0-linux-arm64.patch
COPY workloads/languages/go/antithesis-go-toolexec-v0.8.0-stdlib.patch /build/stdlib.patch
COPY workloads/languages/go/configure-instrumentation.sh /build/configure-instrumentation.sh

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

RUN mkdir -p /out/minio /out/symbols /out/stdlib \
    && sdk_dir=/build/antithesis-sdk-go \
    && version="$(echo "${MINIO_VERSION#RELEASE.}" | sed -E 's/T([0-9]+)-([0-9]+)-([0-9]+)Z$/T\1:\2:\3Z/')" \
    && (cd /build/minio \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           bash /build/configure-instrumentation.sh "${sdk_dir}" /out/stdlib . > /out/stdlib/instrumentation.env \
        && source /out/stdlib/instrumentation.env \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           ANTITHESIS_SDK_MODULE_DIR="${sdk_dir}" \
           ANTITHESIS_SYMBOLS_DIR=/out/symbols \
           ANTITHESIS_SYMBOL_PREFIX=minio \
           go build -toolexec=antithesis-go-toolexec -buildvcs=false \
             -ldflags "-X github.com/minio/minio/cmd.Version=${version} -X github.com/minio/minio/cmd.ReleaseTag=${MINIO_VERSION} -X github.com/minio/minio/cmd.CopyrightYear=2025" \
             -o /out/minio/minio .) \
    && test -x /out/minio/minio \
    && set -- /out/symbols/*.sym.tsv \
    && test -s "$1" \
    && grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/minio/minio \
    && sha256sum /out/minio/minio \
       | awk '{print $1 "  /opt/minio/minio"}' > /out/harmony-instrumented-events \
    && cp /build/minio/LICENSE /out/MINIO-LICENSE \
    && cp "${sdk_dir}/LICENSE" /out/ANTITHESIS-SDK-GO-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG MINIO_VERSION=RELEASE.2025-10-15T17-29-55Z
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

COPY --from=instrumented-build /out/minio/minio /opt/minio/minio
COPY --from=harness-build /out/minio-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/*.sym.tsv /symbols/
COPY --from=instrumented-build /out/stdlib /symbols/stdlib
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/MINIO-LICENSE /licenses/minio.txt
COPY --from=instrumented-build /out/ANTITHESIS-SDK-GO-LICENSE /licenses/antithesis-sdk-go.txt

RUN chmod 0755 /opt/minio/minio /opt/harmony/minio-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -x /opt/minio/minio \
    && test -x /opt/harmony/minio-harness \
    && set -- /symbols/*.sym.tsv \
    && test -s "$1" \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/minio.txt \
    && test -s /licenses/antithesis-sdk-go.txt

LABEL org.opencontainers.image.title="harmony MinIO harness workload"
LABEL org.opencontainers.image.version="${MINIO_VERSION}"
LABEL org.opencontainers.image.description="upstream MinIO source built through the Antithesis Go instrumentation pipeline, driven by the Harmony harness kit"

ARG HARNESS_BUNDLE=bundle
COPY workloads/harness/targets/minio/${HARNESS_BUNDLE} /etc/harmony/bundle
COPY workloads/harness/targets/minio/setup.sh workloads/harness/targets/minio/node.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1"]
