# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.24-bookworm@sha256:1a6d4452c65dea36aac2e2d606b01b4a029ec90cc1ae53890540ce6173ea77ac
FROM ${GO_IMAGE} AS compiled
ARG ANTITHESIS_SDK_VERSION=0.8.0
ENV CGO_ENABLED=1 GOFLAGS=-trimpath
RUN apt-get update && apt-get install --yes --no-install-recommends gcc libc6-dev patch \
    && rm -rf /var/lib/apt/lists/*
RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}"
COPY workloads/bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch /build/arm64.patch
COPY workloads/languages/go/antithesis-go-toolexec-v0.8.0-stdlib.patch /build/stdlib.patch
COPY workloads/languages/go/configure-stdlib.sh /build/configure-stdlib.sh
RUN sdk_cache_dir="$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && mkdir -p /build/sdk && cp -a "${sdk_cache_dir}/." /build/sdk/ && chmod -R u+w /build/sdk \
    && patch --batch --forward --fuzz=0 -d /build/sdk -p1 < /build/arm64.patch \
    && patch --batch --forward --fuzz=0 -d /build/sdk -p1 < /build/stdlib.patch \
    && (cd /build/sdk && gofmt -w tools/antithesis-go-toolexec/config.go tools/antithesis-go-toolexec/policy.go tools/antithesis-go-toolexec/harmony_std_test.go \
        && go test ./tools/antithesis-go-toolexec && go install ./tools/antithesis-go-toolexec)
WORKDIR /build/fixture
COPY consonance/harmony-linux/linux/go-runtime-guest/go.mod ./
COPY consonance/harmony-linux/linux/go-runtime-guest/cmd/language-fixture ./cmd/language-fixture
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]
RUN mkdir -p /out/symbols /out/stdlib \
    && bash /build/configure-stdlib.sh /build/sdk harmony-go-runtime-proof /out/stdlib > /out/stdlib/instrumentation.env \
    && source /out/stdlib/instrumentation.env \
    && ANTITHESIS_SDK_MODULE_DIR=/build/sdk \
       ANTITHESIS_SYMBOLS_DIR=/out/symbols ANTITHESIS_SYMBOL_PREFIX=go-fixture \
       go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/fixture ./cmd/language-fixture \
    && cp /out/fixture /out/symbols/fixture \
    && sha256sum /out/fixture | awk '{print $1 "  /opt/harmony/fixture"}' > /out/symbols/harmony-instrumented-events \
    && cp /build/sdk/LICENSE /out/ANTITHESIS-SDK-GO-LICENSE
FROM ${DEBIAN_IMAGE} AS language-base
COPY --from=compiled /out/fixture /opt/harmony/fixture
COPY --from=compiled /out/symbols /symbols
COPY --from=compiled /out/stdlib /symbols/stdlib
COPY --from=compiled /out/ANTITHESIS-SDK-GO-LICENSE /licenses/antithesis-sdk-go.txt
CMD ["/opt/harmony/fixture"]
