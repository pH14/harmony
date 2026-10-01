# SPDX-License-Identifier: AGPL-3.0-or-later
ARG ETCD_BUILD_IMAGE=harmony-go-etcd-build:local
ARG ETCD_RUNTIME_IMAGE=harmony-etcd-language:local
FROM ${ETCD_BUILD_IMAGE} AS comparison
WORKDIR /build/etcd/server
RUN CGO_ENABLED=1 GOFLAGS=-trimpath go build -buildvcs=false -o /out/etcd-plain .
FROM ${ETCD_RUNTIME_IMAGE}
RUN apt-get update && apt-get install --yes --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=comparison /out/etcd-plain /opt/etcd/etcd-plain
COPY workloads/languages/go/measure-etcd.py /opt/harmony/measure-etcd.py
CMD ["python3", "/opt/harmony/measure-etcd.py"]
