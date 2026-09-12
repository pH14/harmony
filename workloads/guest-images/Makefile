# SPDX-License-Identifier: AGPL-3.0-or-later
# Workload image recipes. Platform kernels and the workload-free OCI runtime
# are owned by consonance/harmony-linux; this Makefile owns application inputs.

.PHONY: fetch postgres-image arm64-postgres-kernel arm64-postgres-image \
    campaign-image order-image uuid-image docker-image k3s-image

fetch:
	$(MAKE) -C ../../consonance/harmony-linux fetch
	./fetch.sh

postgres-image:
	./build-postgres-image.sh

arm64-postgres-kernel:
	$(MAKE) -C ../../consonance/harmony-linux/linux arm64-kernel

arm64-postgres-image: arm64-postgres-kernel
	./build-arm64-postgres-image.sh

campaign-image:
	./build-campaign-image.sh

order-image:
	./build-order-image.sh

uuid-image:
	./build-uuid-image.sh

docker-image:
	./build-docker-image.sh

k3s-image:
	./build-k3s-image.sh
