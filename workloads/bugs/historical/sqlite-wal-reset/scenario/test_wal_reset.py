#!/usr/bin/env python3
"""Run the WAL reset race as a deterministic Harmony scenario."""

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "faults" / "python"))
from harmony_scenario import Scenario


CHECKPOINT_SITE = 0x514C0001
LOSS_ASSERTION = "no-lost-committed-writes"


def scenario(image: Path, arguments: argparse.Namespace) -> Scenario:
    return (
        Scenario(
            image=image,
            harmony=arguments.harmony,
            kernel=arguments.kernel,
            base_initramfs=arguments.base_initramfs,
            ram_mib=arguments.ram_mib,
        )
        .park_site(node=0, site=CHECKPOINT_SITE, hold_ms=10000, then_wait_ms=100)
        .hook(1, then_wait_ms=100)
        .wait(12000)
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--fixed-image", type=Path)
    parser.add_argument("--harmony", type=Path, default=Path("harmony"))
    parser.add_argument("--kernel", type=Path)
    parser.add_argument("--base-initramfs", type=Path)
    parser.add_argument("--ram-mib", type=int, default=1024)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    affected = scenario(args.image, args).run(args.out / "affected")
    (
        affected.reached_site(CHECKPOINT_SITE)
        .observed("wal-reset-before-checkpoint")
        .observed("stale-backfill-advanced")
        .observed("final-canary-read-completed")
        .violated(LOSS_ASSERTION)
        .identical_replays()
    )
    print(f"affected version reproduced loss: {affected.directory}")

    if args.fixed_image is not None:
        fixed = scenario(args.fixed_image, args).run(args.out / "fixed")
        (
            fixed.reached_site(CHECKPOINT_SITE)
            .observed("wal-reset-before-checkpoint")
            .observed("final-canary-read-completed")
            .not_observed("stale-backfill-advanced")
            .clean()
            .identical_replays()
        )
        print(f"fixed version remained clean: {fixed.directory}")


if __name__ == "__main__":
    main()
