#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check a dump from the real Rust preparation API; never generate approvals."""
import argparse
import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import struct
import subprocess

SPEC = importlib.util.spec_from_file_location("scanner", Path(__file__).resolve().parents[2] / "consonance/harmony-linux/scripts/x86-xstate-admission.py")
a = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(a)
FILES = {"session-config.json", "kernel.bin", "platform.cpio.gz", "rootfs.cpio.gz", "control.cpio.gz", "execution.json", "image-runtime-config.json", "image-inputs.json", "composed-initramfs.bin"}
COMPONENTS = ["platform.cpio.gz", "rootfs.cpio.gz", "control.cpio.gz"]

ORACLE_SCOPE = "nova-ae-linux-x86_64-kvm-oracle-v1"
ORACLE_SEED = 0x4e4f56415f434931
ORACLE_CMDLINE = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1 rdinit=/init"


def inspect_engine(manifest, session):
    scope = manifest.get("engine_scope")
    if scope == "default-linux-x86_64-kvm-session":
        if "oracle" in manifest:
            raise a.Rejected("oracle metadata cannot use default-session scope")
        return
    if scope != ORACLE_SCOPE or manifest.get("mode") != "nes":
        raise a.Rejected("unsupported engine/default session scope")
    expected_session = {"ram_bytes": 134217728, "seed": ORACLE_SEED,
                        "run_budget": 2000000000, "cmdline": ORACLE_CMDLINE,
                        "identity_tag": "", "wall_limit": None,
                        "defer_virtual_time_checkpoint_hashes": True}
    if session != expected_session:
        raise a.Rejected("unsupported Nova A–E oracle configuration")
    expected = {"version": 1, "backend": "boot_linux_stock_virtual_time",
                "control": "direct-ControlServer-A-E",
                "restore_mode": "in-place-with-remap-factory",
                "deadline_kind": "absolute-vtime", "setup_payloads": [[0, 1]] * 16,
                "tree_seed": ORACLE_SEED ^ 0x4954454d325f5452}
    oracle = manifest.get("oracle")
    if not isinstance(oracle, dict) or set(oracle) != set(expected):
        raise a.Rejected("unsupported Nova A–E oracle metadata")
    if any(oracle[k] != v for k, v in expected.items()):
        raise a.Rejected("unsupported Nova A–E oracle controls")


def archive_entries(data):
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as source:
        raw = source.read(a.MAX_TREE + 1)
    if len(raw) > a.MAX_TREE:
        raise a.Rejected("component exceeds decompression bound")
    return a.parse_newc(raw)


def encode_newc(entries):
    output = bytearray()
    kinds = {"file": stat.S_IFREG, "directory": stat.S_IFDIR, "symlink": stat.S_IFLNK}
    for item, content in entries:
        if item["kind"] not in kinds:
            raise a.Rejected("workload root contains unsupported special entry")
        name = (item["path"].lstrip("/") or ".").encode() + b"\0"
        fields = [item["inode"], kinds[item["kind"]] | item["mode"], item["uid"], item["gid"], item["nlink"], item["mtime"], len(content), *item["archive_device"], *item["rdev"], len(name), 0]
        output.extend(b"070701" + b"".join(f"{v:08x}".encode() for v in fields))
        output.extend(name)
        output.extend(bytes((-len(output)) % 4))
        output.extend(content)
        output.extend(bytes((-len(output)) % 4))
    name = b"TRAILER!!!\0"
    fields = [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, len(name), 0]
    output.extend(b"070701" + b"".join(f"{v:08x}".encode() for v in fields) + name)
    output.extend(bytes((-len(output)) % 4))
    return bytes(output)


def inspect_dump(directory):
    if (directory / "manifest.json").is_symlink():
        raise a.Rejected("manifest must not be a symlink")
    manifest = json.loads(a.bounded_read(directory / "manifest.json"))
    if manifest.get("version") != 1 or manifest.get("mode") not in ("nes", "postgres") or set(manifest.get("files", {})) != FILES:
        raise a.Rejected("unsupported dump manifest")
    blobs = {}
    for name in FILES:
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise a.Rejected(f"dump component is not a regular file: {name}")
        data = a.bounded_read(path)
        if manifest["files"][name] != {"size": len(data), "sha256": a.digest(data)}:
            raise a.Rejected(f"dump file hash/size differs: {name}")
        blobs[name] = data
    session = json.loads(blobs["session-config.json"])
    inspect_engine(manifest, session)
    tokens = session["cmdline"].split()
    if [token for token in tokens if token.startswith("LD_BIND_NOW=")] != ["LD_BIND_NOW=1"] or not {"noxsaveopt", "noxsaves", "rdinit=/init"}.issubset(tokens):
        raise a.Rejected("required pre-PID1 binding/kernel configuration is absent")
    offset, composition = 0, []
    for name in COMPONENTS:
        composition.append({"file": name, "offset": offset, "size": len(blobs[name])})
        offset += len(blobs[name])
    if composition != manifest.get("composition") or b"".join(blobs[n] for n in COMPONENTS) != blobs["composed-initramfs.bin"]:
        raise a.Rejected("composition order/offset/bytes differ")
    digest = hashlib.sha256(b"harmony-oci-prepared-execution-v1\0")
    for name in ["rootfs.cpio.gz", "control.cpio.gz", "execution.json"]:
        digest.update(struct.pack("<Q", len(blobs[name])))
        digest.update(blobs[name])
    if digest.hexdigest() != manifest.get("prepared_identity"):
        raise a.Rejected("prepared API identity differs")
    parts = [archive_entries(blobs[name]) for name in COMPONENTS]
    for item, _ in parts[0]:
        if item["path"].startswith("/harmony-oci/") and not (item["path"] == "/harmony-oci/rootfs" and item["kind"] == "directory"):
            raise a.Rejected("platform writes into prepared-execution namespace")
    seen = {}
    for entries in parts:
        for item, _ in entries:
            old = seen.get(item["path"])
            if old and (old["kind"] != "directory" or item["kind"] != "directory" or any(old[k] != item[k] for k in ("mode", "uid", "gid", "mtime"))):
                raise a.Rejected(f"unsupported archive collision: {item['path']}")
            seen[item["path"]] = item
    root_entries = []
    for item, content in parts[1]:
        if item["path"] == "/harmony-oci" and item["kind"] == "directory":
            continue
        prefix = "/harmony-oci/rootfs"
        if item["path"] != prefix and not item["path"].startswith(prefix + "/"):
            raise a.Rejected("rootfs segment writes outside workload root")
        root_entries.append((dict(item, path=item["path"][len(prefix):] or "/"), content))
    control = {item["path"]: (item, content) for item, content in parts[2]}
    config_path, execution_path = "/harmony-oci/config.json", "/harmony-oci/execution.json"
    if control.get(execution_path, (None, None))[1] != blobs["execution.json"]:
        raise a.Rejected("actual control execution JSON differs from dump")
    config = json.loads(control[config_path][1])
    execution = json.loads(blobs["execution.json"])
    if config["process"]["args"] != ["/usr/lib/harmony/supervisor"] or config["root"] != {"path": "rootfs", "readonly": False}:
        raise a.Rejected("unsupported runtime composition")
    for env in (config["process"]["env"], execution["env"]):
        if [v for v in env if v.startswith("LD_BIND_NOW=")] != ["LD_BIND_NOW=1"] or any(v.startswith("LD_") and not v.startswith("LD_BIND_NOW=") or v.startswith("GLIBC_TUNABLES=") for v in env):
            raise a.Rejected("unreviewed loader environment")
    expected_external = {"/game.nes"} if manifest["mode"] == "nes" else set()
    if set(manifest.get("external_inputs", {})) != expected_external:
        raise a.Rejected("unsupported external input set")
    actual_control_files = {path for path, (item, _) in control.items() if item["kind"] == "file"}
    expected_control_files = {config_path, execution_path} | {"/harmony-oci/external" + path for path in expected_external}
    if actual_control_files != expected_control_files:
        raise a.Rejected("missing or unexpected control input file")
    for path, (item, content) in control.items():
        if item["kind"] == "directory" and (path == "/harmony-oci" or path == "/harmony-oci/external"):
            continue
        if path in (config_path, execution_path) and item["kind"] == "file":
            continue
        external = path.removeprefix("/harmony-oci/external")
        if external not in expected_external or item["kind"] != "file" or content.startswith(b"\x7fELF"):
            raise a.Rejected(f"unsupported control/code input: {path}")
        if manifest["external_inputs"][external] != {"size": len(content), "sha256": a.digest(content)}:
            raise a.Rejected("external input digest differs")
        mounts = [m for m in config["mounts"] if m.get("destination") == external]
        if len(mounts) != 1 or mounts[0].get("source") != path or mounts[0].get("options") != ["bind", "ro"]:
            raise a.Rejected("external input mount is not canonical read-only binding")
    root_files = {i["path"]: data for i, data in root_entries if i["kind"] == "file"}
    required_code = {"/workload.sql", "/usr/local/bin/postgres-workload.sh"} if manifest["mode"] == "postgres" else set()
    if set(manifest.get("code_inputs", {})) != required_code:
        raise a.Rejected("unsupported code/SQL input set")
    for path in required_code:
        data = root_files[path]
        if manifest["code_inputs"][path] != {"size": len(data), "sha256": a.digest(data)}:
            raise a.Rejected("code/SQL input digest differs")
    if manifest["mode"] == "nes" and execution["argv"] != ["/opt/harmony/play-agent", "--nes-payload", "--rom", "/game.nes"]:
        raise a.Rejected("unsupported NES argv")
    if manifest["mode"] == "postgres" and execution["argv"] != ["/usr/local/bin/postgres-workload.sh"]:
        raise a.Rejected("unsupported PostgreSQL argv")
    report = {"version": 1, "admitted": False, "prepared_identity": manifest["prepared_identity"], "execution": execution, "session_config": session, "engine_scope": manifest["engine_scope"], "oracle": manifest.get("oracle"), "runtime_config": config, "limitations": ["Controlled trusted code only; writable OCI root is not runtime immutability enforcement.", "Each independent archive is parsed separately; no general concatenated-archive overlay support."]}
    return report, encode_newc(root_entries)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["inventory", "verify"])
    parser.add_argument("dump", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--objdump", default="objdump")
    args = parser.parse_args()
    created = False
    try:
        if args.output.resolve().is_relative_to(args.dump.resolve()):
            raise a.Rejected("output must be outside dump")
        args.output.mkdir(exist_ok=False)
        created = True
        report, root = inspect_dump(args.dump)
        root_path = args.output / "workload-root.cpio"
        root_path.write_bytes(root)
        scans = {}
        for name, path in [("platform", args.dump / "platform.cpio.gz"), ("workload", root_path)]:
            scans[name] = a.inventory_initramfs(path, args.objdump)
            (args.output / (name + "-candidate.json")).write_text(json.dumps(scans[name][0], indent=2))
        if args.mode == "verify":
            for name in ("platform", "workload"):
                if not a.verify(scans[name][0]):
                    raise a.Rejected(f"{name} rejected: {scans[name][0]['errors']}")
                (args.output / (name + "-verification.json")).write_text(json.dumps(scans[name][0], indent=2))
            report["admitted"] = True
        (args.output / "composition-report.json").write_text(json.dumps(report, indent=2))
        return 0
    except (OSError, ValueError, KeyError, TypeError, AttributeError, EOFError, subprocess.SubprocessError) as error:
        if created:
            (args.output / "composition-report.json").write_text(json.dumps({"version": 1, "admitted": False, "errors": [str(error)]}, indent=2))
        print(f"prepared admission rejected: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
