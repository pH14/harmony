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
COPY workloads/harness/targets/redisraft/go.mod workloads/harness/targets/redisraft/go.sum targets/redisraft/
RUN (cd kit && go mod download) && (cd targets/redisraft && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/redisraft/main.go targets/redisraft/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/redisraft && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/redisraft-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && test -x /out/redisraft-harness

FROM ${DEBIAN_IMAGE} AS instrumented-build

ARG REDIS_COMMIT=f88f8661ac29c054cffb795e6c371fbb2240aa0e
ARG REDISRAFT_COMMIT=dfd91d4af30cec6bd66131e4ba837a7256ba2480
ARG REDIS_REPO=https://github.com/redis/redis.git
ARG REDISRAFT_REPO=https://github.com/RedisLabs/redisraft.git
ARG RAFT_REPO=https://github.com/RedisLabs/raft.git
ARG HIREDIS_REPO=https://github.com/redis/hiredis.git
ARG LIBUV_REPO=https://github.com/libuv/libuv.git

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        autoconf automake binutils ca-certificates clang git libbsd-dev libc6-dev libtool make pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
# A full commit hash pins each source tree. The redisraft tree records the
# commits of its raft, hiredis and libuv submodules.
RUN fetch() { mkdir -p "$1" && git -C "$1" init -q && git -C "$1" fetch -q --depth 1 "$2" "$3" \
        && git -C "$1" checkout -q FETCH_HEAD && test "$(git -C "$1" rev-parse HEAD)" = "$3"; } \
    && pin() { git -C redisraft ls-tree HEAD "deps/$1" | awk '$2 == "commit" {print $3}'; } \
    && fetch redis "${REDIS_REPO}" "${REDIS_COMMIT}" \
    && fetch redisraft "${REDISRAFT_REPO}" "${REDISRAFT_COMMIT}" \
    && fetch raft "${RAFT_REPO}" "$(pin raft)" \
    && fetch hiredis "${HIREDIS_REPO}" "$(pin hiredis)" \
    && fetch libuv "${LIBUV_REPO}" "$(pin libuv)" \
    && for tree in redis redisraft raft hiredis libuv; do echo "${tree} $(git -C "${tree}" rev-parse HEAD)"; done > /build/sources

COPY workloads/languages/c/shim.c /build/languages/c/shim.c
COPY workloads/languages/vendor /build/languages/vendor

ENV CC=clang
RUN cover="-O1 -g -fsanitize-coverage=trace-pc-guard" \
    && clang -O1 -g -fPIC -c /build/languages/c/shim.c -o /build/shim.o \
    && mkdir -p /build/deps/lib /build/deps/include \
    && (cd libuv && sh autogen.sh \
        && ./configure --with-pic --disable-shared --prefix=/build/deps CFLAGS="-O1 -g" \
        && make -j"$(nproc)" CFLAGS="${cover}" && make install) \
    && make -C hiredis -j"$(nproc)" OPTIMIZATION=-O1 CFLAGS="-fsanitize-coverage=trace-pc-guard" libhiredis.a \
    && mkdir -p /build/deps/include/hiredis/adapters \
    && cp hiredis/*.h /build/deps/include/hiredis/ \
    && cp hiredis/adapters/*.h /build/deps/include/hiredis/adapters/ \
    && cp hiredis/libhiredis.a /build/deps/lib/ \
    && (cd raft && for f in raft_server raft_server_properties raft_node raft_log; do \
            clang ${cover} -fPIC -fno-common -fsigned-char -Iinclude -c "src/${f}.c" -o "src/${f}.o"; \
        done && ar -rc /build/deps/lib/libraft.a src/*.o && cp include/raft.h include/raft_types.h /build/deps/include/)
RUN make -C redis/src -j"$(nproc)" MALLOC=libc BUILD_TLS=no OPTIMIZATION=-O1 \
        REDIS_CFLAGS="-fsanitize-coverage=trace-pc-guard" REDIS_LDFLAGS="/build/shim.o -Wl,--build-id" redis-server
RUN cd redisraft && for f in redisraft common node util config raft snapshot log proxy serialization; do \
            clang -O1 -g -fsanitize-coverage=trace-pc-guard -std=c99 -fPIC -fcommon \
                -D_POSIX_C_SOURCE=200112L -D_GNU_SOURCE -I/build/deps/include -c "${f}.c" -o "${f}.o"; \
        done \
    && clang -shared -Wl,-Bsymbolic-functions -Wl,--build-id -o redisraft.so \
        redisraft.o common.o node.o util.o config.o raft.o snapshot.o log.o proxy.o serialization.o \
        /build/deps/lib/libraft.a /build/deps/lib/libhiredis.a /build/deps/lib/libuv.a -lpthread -ldl \
    && cd /build \
    && nm -D redis/src/redis-server | grep -c ' T __sanitizer_cov_trace_pc_guard$' \
    && nm redis/src/redis-server | grep -c antithesis_load_libvoidstar \
    && nm -u redisraft/redisraft.so | grep -c __sanitizer_cov_trace_pc_guard

RUN mkdir -p /out/redis /out/symbols \
    && cp redis/src/redis-server redisraft/redisraft.so /out/redis/ \
    && cp /out/redis/redis-server /out/redis/redisraft.so /out/symbols/ \
    && for f in redis-server redisraft.so; do \
        nm --defined-only "/out/redis/${f}" | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > "/out/symbols/${f}.sym.tsv" \
        && test -s "/out/symbols/${f}.sym.tsv" \
        && sha256sum "/out/redis/${f}" | awk -v path="/opt/redis/${f}" '{print $1 "  " path}' >> /out/symbols/harmony-instrumented-events; \
    done \
    && cp /build/sources /out/symbols/sources \
    && cp redis/COPYING /out/REDIS-LICENSE \
    && cp redisraft/LICENSE /out/REDISRAFT-LICENSE \
    && cp raft/LICENSE /out/RAFT-LICENSE \
    && cp hiredis/COPYING /out/HIREDIS-LICENSE \
    && cp libuv/LICENSE /out/LIBUV-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG REDISRAFT_COMMIT=dfd91d4af30cec6bd66131e4ba837a7256ba2480
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

COPY --from=instrumented-build /out/redis /opt/redis
COPY --from=harness-build /out/redisraft-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols /symbols
COPY --from=instrumented-build /out/REDIS-LICENSE /licenses/redis.txt
COPY --from=instrumented-build /out/REDISRAFT-LICENSE /licenses/redisraft.txt
COPY --from=instrumented-build /out/RAFT-LICENSE /licenses/raft.txt
COPY --from=instrumented-build /out/HIREDIS-LICENSE /licenses/hiredis.txt
COPY --from=instrumented-build /out/LIBUV-LICENSE /licenses/libuv.txt
COPY workloads/languages/vendor/ANTITHESIS-SDK-CPP-LICENSE /licenses/antithesis-sdk-cpp.txt

RUN chmod 0755 /opt/redis/redis-server /opt/harmony/redisraft-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -s /symbols/redis-server.sym.tsv \
    && test -s /symbols/redisraft.so.sym.tsv \
    && test "$(wc -l < /symbols/harmony-instrumented-events)" -eq 2

LABEL org.opencontainers.image.title="harmony Redis-Raft harness workload"
LABEL org.opencontainers.image.version="${REDISRAFT_COMMIT}"
LABEL org.opencontainers.image.description="upstream Redis and Redis-Raft built with clang trace-pc-guard coverage, driven by the Harmony harness kit"

COPY workloads/harness/targets/redisraft/bundle /etc/harmony/bundle
COPY workloads/harness/targets/redisraft/setup.sh workloads/harness/targets/redisraft/node.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1"]
