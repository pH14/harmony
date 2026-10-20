# SPDX-License-Identifier: AGPL-3.0-or-later
ARG GO_IMAGE=golang:1.26-bookworm@sha256:d9c68c2c51161e12fd77e4c6320687c9cd86e1af1e3ad6e6cd63ff970641453c
ARG BASE_IMAGE=localhost/nats-kv:base

FROM ${GO_IMAGE} AS probe
ENV GOTOOLCHAIN=local
WORKDIR /src
COPY .github/clock-validation/clockprobe.go main.go
RUN go mod init clockprobe \
    && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 GOFLAGS=-trimpath \
       go build -buildvcs=false -o /out/clockprobe .

FROM ${BASE_IMAGE}
COPY --from=probe /out/clockprobe /opt/harmony/clockprobe
COPY .github/clock-validation/bundle /etc/harmony/bundle
RUN chmod 0755 /opt/harmony/clockprobe
