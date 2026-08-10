#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-6 masked-register-digest lane checker (bead hm-3bwm) — the named condition on
# upgrading the AA-6 LinuxGuest disposition from PROVISIONAL to full GO.
#
# The fourth ratified AA-6 gate-semantics change narrowed the LinuxGuest compared digest
# to `console + vGIC`, dropping the register file because x29/SP carry the userspace init's
# stack-placement ASLR (the AA-5(c) entropy residual, hm-of6t F12). This lane runs the AA-6
# INJECTION configuration for >=1000 same-seed reps and confirms, at gate scale, that the
# narrowing masks EXACTLY-AND-ONLY {x29, SP}: the masked-register digest (the full register
# file minus exactly {x29, SP}, host-time counters already excluded) is bit-identical across
# every rep, and so is the injection-Moment register witness (hm-fiqo's `injected_landed_digest`).
#
#   aa6-masked-digest-check.py --run-dir <dir> --min-reps 1000 --out verdict.json
#
# Disposition (per tasks/138):
#   * All reps identical  => named condition MET (RESULT: PASS).
#   * ANY divergence in the masked digest (which excludes {x29, SP}) => a register OTHER than
#     {x29, SP} diverged — a possible injection-path register divergence the console+vGIC
#     narrowing was masking. That is a P0-class STOP: this checker FAILS and enumerates the
#     distinct digests (never hides them), and the operator PARKs + escalates. The mask is a
#     closed list; a divergence is NEVER "fixed" by widening it or narrowing the digest.
#
# Each rep's `<dir>/rep-NNNN.stdout` is the harness summary line; the masked digest exists
# only machine-side (a KVM register read), so unlike the console it cannot be recomputed from
# a retained artifact — it is compared across reps and required to be well-formed and, for the
# injected witness, non-`none` (the injection must have fired, or the lane is vacuous).
import argparse
import json
import re
import sys
from pathlib import Path

# A rep summary line is a few kilobytes; cap the read so a malformed/oversized run dir cannot
# OOM the checker before it validates anything (the aa5-identity-check F6 discipline).
MAX_REP_FILE = 64 * 1024 * 1024  # 64 MiB

SHA256 = re.compile(r"sha256:[0-9a-f]{64}")

# The pinned pinned-image AA-5(c)/AA-6 LinuxGuest artifacts (results/aa-5/live-20260721): the
# lane must boot exactly these, so a stray unpinned build cannot masquerade as the gate.
EXPECTED_IMAGE_SHA256 = "d0161a7d41309b6e9139534d99c8c3d24152c0b10c06b4829443402698c5aefe"
EXPECTED_INITRAMFS_SHA256 = "604733be3338ac55cc0f387ba55b7b6b31250d158761ca2cc422cf2e37d08573"

# The mask is a CLOSED LIST OF TWO, enumerated (not implied) in every rep's summary line via
# `masked_excluded_gprs`. These full KVM ids are pinned to the on-N1 per-register dump
# (results/aa-6/live-20260721/linuxguest-regs-divergence.diff): x29 = 0x...003A, SP = 0x...003E.
EXPECTED_MASKED_GPRS = "x29:0x603000000010003a,SP:0x603000000010003e"
# The pre-existing host-time counter exclusion, by name (is_host_time_register), enumerated too.
EXPECTED_MASKED_HOST_TIME = "CNTPCT_EL0,CNTPCTSS_EL0,CNTVCTSS_EL0,KVM_REG_ARM_TIMER_CNT"

# The AA-6 injection configuration the lane runs (host/aa6-masked-digest-lane.sh): PPI 22, the
# UNWIRED interrupt, fired at the first exact refresh landing (--inject-at-work 1). The harness
# STAMPS this per rep in the summary line (`injection_enabled`/`inject_ppi`/`inject_at_work`),
# written from the config it actually executed. This checker enforces the STAMPED fields — the
# same attestation the floor checker's `aa6-matrix` reads out of `run-set.json` — so the two
# cannot disagree about whether injection ran (bead hm-oh3v routes both through one stamp).
EXPECTED_INJECT_PPI = "22"
EXPECTED_INJECT_AT_WORK = "1"

# Every field the lane relies on must be present in each rep line, or the rep is malformed.
REQUIRED_KEYS = [
    "image_sha256",
    "initramfs_sha256",
    "state_digest",
    "masked_regs_digest",
    "injected_landed_digest",
    "injection_enabled",
    "inject_ppi",
    "inject_at_work",
    "masked_excluded_gprs",
    "masked_excluded_host_time",
]


def _read_bounded(path: Path) -> str:
    size = path.stat().st_size
    if size > MAX_REP_FILE:
        raise SystemExit(f"FAIL: {path}: {size} bytes exceeds the {MAX_REP_FILE}-byte rep-file cap")
    return path.read_text()


def load_rep(path: Path) -> dict:
    fields = dict(re.findall(r"([a-z_0-9]+)=([^\s]+)", _read_bounded(path)))
    missing = [k for k in REQUIRED_KEYS if k not in fields]
    if missing:
        raise SystemExit(f"FAIL: {path}: summary line missing {missing}")
    return fields


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", required=True, type=Path)
    ap.add_argument("--min-reps", type=int, default=1000)
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()

    rep_paths = sorted(args.run_dir.glob("rep-*.stdout"))
    checks: list[dict] = []

    def check(name: str, ok: bool, detail: str) -> None:
        checks.append({"check": name, "ok": bool(ok), "detail": detail})

    # rep-count floor (the >=1000 gate).
    check(
        "rep-floor",
        len(rep_paths) >= args.min_reps,
        f"{len(rep_paths)} reps present, floor {args.min_reps}",
    )

    masked_digests: dict[str, int] = {}
    witness_digests: dict[str, int] = {}
    image_shas: set[str] = set()
    initramfs_shas: set[str] = set()
    masked_gprs: set[str] = set()
    masked_host_time: set[str] = set()
    injection_enabled: set[str] = set()
    inject_ppi: set[str] = set()
    inject_at_work: set[str] = set()
    malformed: list[str] = []
    witness_none = 0

    for path in rep_paths:
        f = load_rep(path)
        image_shas.add(f["image_sha256"])
        initramfs_shas.add(f["initramfs_sha256"])
        masked_gprs.add(f["masked_excluded_gprs"])
        masked_host_time.add(f["masked_excluded_host_time"])
        injection_enabled.add(f["injection_enabled"])
        inject_ppi.add(f["inject_ppi"])
        inject_at_work.add(f["inject_at_work"])

        md = f["masked_regs_digest"]
        wd = f["injected_landed_digest"]
        if not SHA256.fullmatch(md):
            malformed.append(f"{path.name}: masked_regs_digest={md}")
        else:
            masked_digests[md] = masked_digests.get(md, 0) + 1
        if wd == "none":
            witness_none += 1
        elif not SHA256.fullmatch(wd):
            malformed.append(f"{path.name}: injected_landed_digest={wd}")
        else:
            witness_digests[wd] = witness_digests.get(wd, 0) + 1

    check("reps-present", bool(rep_paths), f"{len(rep_paths)} rep-*.stdout files")
    check("well-formed-digests", not malformed, "; ".join(malformed[:8]) or "all digests well-formed")

    # The artifacts are the pinned pinned-image AA-5(c)/AA-6 LinuxGuest, identical across reps.
    check(
        "image-pins",
        image_shas == {EXPECTED_IMAGE_SHA256} and initramfs_shas == {EXPECTED_INITRAMFS_SHA256},
        f"image={sorted(image_shas)} initramfs={sorted(initramfs_shas)}",
    )

    # The mask is enumerated and EXACTLY {x29, SP} (+ the named host-time set), not implied.
    check(
        "mask-enumerated-exactly-x29-sp",
        masked_gprs == {EXPECTED_MASKED_GPRS} and masked_host_time == {EXPECTED_MASKED_HOST_TIME},
        f"gprs={sorted(masked_gprs)} host_time={sorted(masked_host_time)}",
    )

    # The harness-STAMPED injection config attests ON in every rep — the same attestation the
    # floor checker reads from `run-set.json`, so a config slip that left injection OFF fails here
    # as it would there (bead hm-oh3v). Enumerated, not implied: the PPI and at-work index must be
    # single-valued across reps AND the fixed AA-6 config (PPI 22 at the first exact landing).
    check(
        "injection-config-on",
        injection_enabled == {"ON"},
        f"injection_enabled={sorted(injection_enabled)} (every rep's stamped config must be ON)",
    )
    check(
        "injection-config-enumerated",
        inject_ppi == {EXPECTED_INJECT_PPI} and inject_at_work == {EXPECTED_INJECT_AT_WORK},
        f"inject_ppi={sorted(inject_ppi)} inject_at_work={sorted(inject_at_work)} "
        f"(expected ppi {EXPECTED_INJECT_PPI} at-work {EXPECTED_INJECT_AT_WORK})",
    )

    # The injection must have FIRED in every rep (a masked digest over an un-injected boot would
    # be a vacuous negative control, not the AA-6 injection lane). This is the per-rep WITNESS,
    # independent of the stamped config above — the config says ON, the witness says it fired.
    check(
        "injection-fired",
        witness_none == 0 and bool(witness_digests),
        f"{witness_none} reps reported injected_landed_digest=none",
    )

    # THE named condition: the masked digest is bit-identical across all reps. Divergence here
    # is a register OTHER than {x29, SP} moving same-seed — a P0 STOP, enumerated in full.
    check(
        "masked-digest-bit-identical",
        len(masked_digests) == 1,
        _cardinality_detail("masked_regs_digest", masked_digests),
    )
    # The free companion (hm-fiqo): the injection-Moment register witness is bit-identical too.
    check(
        "witness-digest-bit-identical",
        len(witness_digests) == 1,
        _cardinality_detail("injected_landed_digest", witness_digests),
    )

    passed = all(c["ok"] for c in checks)
    verdict = {
        "check": "aa6-masked-register-digest",
        "bead": "hm-3bwm",
        "reps": len(rep_paths),
        "min_reps": args.min_reps,
        "distinct_masked_digests": masked_digests,
        "distinct_witness_digests": witness_digests,
        "checks": checks,
        "result": "PASS" if passed else "FAIL",
    }
    args.out.write_text(json.dumps(verdict, indent=1, sort_keys=True) + "\n")
    n_ok = sum(1 for c in checks if c["ok"])
    print(f"RESULT: {verdict['result']} ({n_ok} of {len(checks)} checks passed) -> {args.out}")
    if not passed:
        for c in checks:
            if not c["ok"]:
                print(f"  [FAIL] {c['check']}: {c['detail']}")
    sys.exit(0 if passed else 1)


def _cardinality_detail(label: str, counts: dict[str, int]) -> str:
    if len(counts) == 1:
        (digest, n), = counts.items()
        return f"all {n} reps agree: {digest}"
    # Enumerate the divergence in full — the P0 evidence, never truncated to a summary.
    parts = ", ".join(f"{d}=[{n}]" for d, n in sorted(counts.items()))
    return f"{len(counts)} DISTINCT {label} values (same seed must be bit-identical): {parts}"


if __name__ == "__main__":
    main()
