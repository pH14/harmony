#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import fcntl
import importlib.util
import os
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path


SEALS = (
    fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL
)


def seal(fd):
    fcntl.fcntl(fd, fcntl.F_ADD_SEALS, SEALS)
    if fcntl.fcntl(fd, fcntl.F_GET_SEALS) & SEALS != SEALS:
        raise ValueError("kernel artifact snapshot could not be sealed")


def write_all(fd, data):
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        if not written:
            raise OSError("short kernel artifact snapshot write")
        view = view[written:]


def snapshot(path, name, fds):
    data = path.read_bytes()
    if not data:
        raise ValueError(f"empty kernel artifact: {path}")
    fd = os.memfd_create(name, os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
    fds.append(fd)
    write_all(fd, data)
    seal(fd)
    return data, f"/proc/{os.getpid()}/fd/{fd}"


def objcopy_bytes(source, flags, fds):
    fd = os.memfd_create("harmony-objcopy-output", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
    fds.append(fd)
    destination = f"/proc/{os.getpid()}/fd/{fd}"
    result = subprocess.run(
        ["objcopy", *flags, source, destination], capture_output=True, text=True,
        check=False,
    )
    if result.returncode:
        raise ValueError(f"objcopy failed for {source}: {result.stderr.strip()}")
    seal(fd)
    size = os.fstat(fd).st_size
    if not size:
        raise ValueError(f"objcopy produced no bytes for {source}")
    return os.pread(fd, size, 0)


def scan_helpers():
    source = Path(__file__).with_name("scan-unclaimed-opcodes.py")
    spec = importlib.util.spec_from_file_location("scan_unclaimed_opcodes", source)
    scanner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(scanner)
    return scanner


def loaded_sections(data, artifact, scanner):
    programs, sections = scanner.headers(data)
    if data[4] == 2:
        program_offset = struct.unpack_from("<Q", data, 32)[0]
        program_size = struct.unpack_from("<H", data, 54)[0]
        physical_offset = 24
        physical_format = "<Q"
    else:
        program_offset = struct.unpack_from("<I", data, 28)[0]
        program_size = struct.unpack_from("<H", data, 42)[0]
        physical_offset = 12
        physical_format = "<I"
    for index, kind, _flags, _offset, address, _size, _memory_size in programs:
        if kind != 1:
            continue
        physical = struct.unpack_from(
            physical_format, data, program_offset + index * program_size + physical_offset
        )[0]
        if physical != address:
            raise ValueError(f"{artifact} PT_LOAD {index} physical/virtual addresses differ")

    loaded = []
    for name, kind, flags, address, offset, size in sections:
        if name == ".rodata..compressed" and kind != 1:
            raise ValueError("decompressor compressed kernel is not a byte section")
        if not flags & 2 or kind == 8 or not size:
            continue
        if offset + size > len(data):
            raise ValueError(f"{artifact} allocated section {name} extends beyond the ELF")
        mapped = [
            index
            for index, load_kind, _flags, load_offset, load_address, load_size, _memory_size
            in programs
            if load_kind == 1 and load_offset <= offset
            and offset + size <= load_offset + load_size
            and address - load_address == offset - load_offset
        ]
        if len(mapped) != 1:
            raise ValueError(f"{artifact} allocated section {name} is not in one loaded PT_LOAD")
        loaded.append((address, address + size, name, offset))
    loaded.sort()
    for left, right in zip(loaded, loaded[1:]):
        if right[0] < left[1]:
            raise ValueError(f"{artifact} allocated sections {left[2]} and {right[2]} overlap")
    if not loaded:
        raise ValueError(f"{artifact} has no loaded allocated byte sections")
    return loaded


def compressed_kernel(decompressor, loaded):
    matches = [
        (address, last - address, offset)
        for address, last, name, offset in loaded
        if name == ".rodata..compressed"
    ]
    if len(matches) != 1:
        raise ValueError("decompressor has no unique loaded compressed kernel")
    address, size, offset = matches[0]
    return address, decompressor[offset : offset + size]


def verify_embedded_kernel(compressed, stripped):
    decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
    try:
        expanded = decoder.decompress(compressed, len(stripped) + 1)
    except zlib.error as error:
        raise ValueError(f"decompressor kernel payload is invalid gzip: {error}") from error
    if not decoder.eof or decoder.unused_data or decoder.unconsumed_tail \
            or expanded != stripped:
        raise ValueError("decompressor kernel payload differs from scanned vmlinux")


def verify_image(setup, decompressor, image, paths, scanner_path, fds):
    result = subprocess.run(["bash", str(scanner_path), paths["vmlinux"], paths["setup"],
                             paths["decompressor"]], check=False)
    if result.returncode:
        raise ValueError("counter-opcode scan rejected a kernel artifact snapshot")

    helper = scan_helpers()
    loaded_sections(setup, "setup", helper)
    decompressor_sections = loaded_sections(decompressor, "decompressor", helper)
    setup_binary = objcopy_bytes(paths["setup"], ["-O", "binary"], fds)
    for position in range(len(setup_binary) - 1):
        hit = helper.counter_encoding(setup_binary, position)
        if hit:
            raise ValueError(
                f"packed setup contains raw {hit[0]} at byte offset 0x{position:x}"
            )
    decompressor_binary = objcopy_bytes(
        paths["decompressor"], ["-O", "binary", "-R", ".note", "-R", ".comment", "-S"], fds
    )
    stripped_kernel = objcopy_bytes(paths["vmlinux"], ["-R", ".comment", "-S"], fds)
    compressed_address, compressed = compressed_kernel(decompressor, decompressor_sections)
    first_address = decompressor_sections[0][0]
    packed_offset = compressed_address - first_address
    if decompressor_binary[packed_offset : packed_offset + len(compressed)] != compressed:
        raise ValueError("packed decompressor kernel payload differs from its loaded section")
    verify_embedded_kernel(compressed, stripped_kernel)

    setup_padding = (-len(setup_binary)) % 4096
    expected = setup_binary + b"\0" * setup_padding + decompressor_binary
    if image != expected:
        raise ValueError("packed bzImage differs from scanned setup/decompressor")
    print("ok: packed bzImage matches sealed, scanned kernel artifacts", flush=True)


def publish(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".harmony-kernel-", dir=path.parent)
    try:
        write_all(fd, data)
        os.fchmod(fd, 0o644)
        os.fsync(fd)
        os.close(fd)
        fd = -1
        os.replace(temporary, path)
    finally:
        if fd >= 0:
            os.close(fd)
        if os.path.exists(temporary):
            os.unlink(temporary)


def main():
    if len(sys.argv) != 6:
        raise ValueError(
            "usage: verify-x86-kernel-image.py <kernel-build> <bzImage-output> "
            "<qualified-bzImage-output> <vmlinux-output> <scan-script>"
        )
    build, image_out, qualified_out, kernel_out, scanner = sys.argv[1:]
    build = Path(build)
    scanner = Path(scanner)
    input_paths = {
        "vmlinux": build / "vmlinux",
        "setup": build / "arch/x86/boot/setup.elf",
        "decompressor": build / "arch/x86/boot/compressed/vmlinux",
        "image": build / "arch/x86/boot/bzImage",
    }
    fds = []
    try:
        contents = {}
        paths = {}
        for name, path in input_paths.items():
            contents[name], paths[name] = snapshot(path, f"harmony-{name}", fds)
        verify_image(
            contents["setup"], contents["decompressor"], contents["image"],
            paths, scanner, fds,
        )
        publish(Path(image_out), contents["image"])
        publish(Path(qualified_out), contents["image"])
        publish(Path(kernel_out), contents["vmlinux"])
        print(f"ok: published qualified x86 kernel at {image_out}")
    finally:
        for fd in reversed(fds):
            os.close(fd)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        raise SystemExit(f"FAIL: x86 kernel image qualification: {error}") from error
