#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import gzip
import importlib.util
import os
import platform
import shutil
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent
VERIFY = ROOT / "verify-x86-kernel-image.py"
SCAN = ROOT / "scan-counter-opcodes.sh"
SCAN_UNCLAIMED = ROOT / "scan-unclaimed-opcodes.py"


def run(*command):
    subprocess.run(command, check=True, capture_output=True)


def fixture(root, setup_sects=7):
    build = root / "kernel-build"
    boot = build / "arch/x86/boot"
    compressed = boot / "compressed"
    compressed.mkdir(parents=True)

    kernel_source = root / "kernel.s"
    kernel_source.write_text('.section .text,"ax"\n.global _start\n_start:\nret\n')
    kernel_linker = root / "kernel.ld"
    kernel_linker.write_text(
        "PHDRS { text PT_LOAD FLAGS(5); }\n"
        "SECTIONS { . = 0x400000; .text : { *(.text) } :text }\n"
    )
    run("as", "--64", "-o", str(root / "kernel.o"), str(kernel_source))
    run("ld", "-nostdlib", "-T", str(kernel_linker), "-e", "_start",
        "-o", str(build / "vmlinux"),
        str(root / "kernel.o"))
    kernel = (build / "vmlinux").read_bytes()
    program_offset = struct.unpack_from("<Q", kernel, 32)[0]
    program_size, program_count = struct.unpack_from("<HH", kernel, 54)
    physical = [
        struct.unpack_from("<Q", kernel, program_offset + index * program_size + 24)[0]
        for index in range(program_count)
        if struct.unpack_from("<I", kernel, program_offset + index * program_size)[0] == 1
    ]
    base = min(physical)
    assert base % 4096 == 0
    (build / ".config").write_text(
        f"CONFIG_PHYSICAL_START=0x{base:x}\nCONFIG_PHYSICAL_ALIGN=0x1000\n"
    )
    run("objcopy", "-R", ".comment", "-S", str(build / "vmlinux"),
        str(root / "stripped-vmlinux"))
    (root / "kernel.gz").write_bytes(
        gzip.compress((root / "stripped-vmlinux").read_bytes(), compresslevel=9, mtime=0)
    )

    setup_source = root / "setup.s"
    setup_source.write_text(
        '.section .text,"ax"\n.global _start\n_start:\nret\n.fill 15,1,0xcc\n'
        f'.section .header,"a",@progbits\n.byte {setup_sects}\n'
    )
    setup_linker = root / "setup.ld"
    setup_linker.write_text(
        "PHDRS { all PT_LOAD FLAGS(5); }\n"
        "SECTIONS { . = 0x400000; .text : { *(.text) } :all "
        ". = 0x4001f1; .header : { *(.header) } :all }\n"
    )
    run("as", "--64", "-o", str(root / "setup.o"), str(setup_source))
    run("ld", "-nostdlib", "-T", str(setup_linker), "-e", "_start",
        "-o", str(boot / "setup.elf"),
        str(root / "setup.o"))

    decompressor_source = root / "decompressor.s"
    decompressor_source.write_text(
        '.section .head.text,"ax"\n.code32\n'
        '.global startup_32\nstartup_32:\nlret\n'
        '.balign 512,0\n.code64\n.global startup_64\nstartup_64:\nret\n'
        '.fill 15,1,0xcc\n.section .rodata..compressed,"a"\n'
        f'.incbin "{root / "kernel.gz"}"\n'
        '.section .text,"ax"\nret\n.fill 15,1,0xcc\n'
    )
    linker = root / "decompressor.ld"
    linker.write_text(
        "PHDRS { all PT_LOAD FLAGS(5); }\n"
        "SECTIONS { . = 0x400000; .head.text : { *(.head.text) } :all "
        ".rodata..compressed : { *(.rodata..compressed) } :all "
        ".text : { *(.text) } :all }\n"
    )
    run("as", "--64", "-o", str(root / "decompressor.o"), str(decompressor_source))
    run("ld", "-nostdlib", "-T", str(linker), "-e", "startup_32", "-o",
        str(compressed / "vmlinux"), str(root / "decompressor.o"))

    run("objcopy", "-O", "binary", str(boot / "setup.elf"), str(boot / "setup.bin"))
    run("objcopy", "-O", "binary", "-R", ".note", "-R", ".comment", "-S",
        str(compressed / "vmlinux"), str(boot / "vmlinux.bin"))
    setup_binary = (boot / "setup.bin").read_bytes()
    assert setup_binary[0x1f1] == setup_sects
    decompressor_binary = (boot / "vmlinux.bin").read_bytes()
    image = setup_binary + b"\0" * ((-len(setup_binary)) % 4096) + decompressor_binary
    (boot / "bzImage").write_bytes(image)
    return build, image


def verify(build, output, env=None):
    return subprocess.run(
        ["python3", str(VERIFY), str(build), str(output / "bzImage"),
         str(output / "x86_64/bzImage"), str(output / "x86_64/bzImage.vmlinux"),
         str(SCAN)], capture_output=True, text=True, env=env,
    )


def add_orphan_section(root, path, name, data):
    content = root / "orphan.bin"
    content.write_bytes(data)
    updated = root / "with-orphan.elf"
    run("objcopy", "--add-section", f"{name}={content}", "--set-section-flags",
        f"{name}=alloc,load,readonly,data,contents", "--change-section-address",
        f"{name}=0x800000", str(path), str(updated))
    updated.replace(path)


@unittest.skipUnless(
    platform.system() == "Linux" and platform.machine() == "x86_64"
    and all(shutil.which(name) for name in ("as", "ld", "objcopy", "objdump", "readelf")),
    "native x86 Linux binutils required",
)
class X86ImageIdentity(unittest.TestCase):
    def test_publishes_exact_scanned_image(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, image = fixture(root)
            output = root / "published"
            result = verify(build, output)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((output / "bzImage").read_bytes(), image)
            self.assertEqual((output / "x86_64/bzImage").read_bytes(), image)
            self.assertEqual(
                (output / "x86_64/bzImage.vmlinux").read_bytes(),
                (build / "vmlinux").read_bytes(),
            )

    def test_rejects_setup_header_with_wrong_packed_decompressor_offset(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, image = fixture(root, setup_sects=6)
            self.assertEqual(image[0x1f1], 6)
            self.assertEqual(image[4096:4098], b"\xcb\x00")
            output = root / "published"
            result = verify(build, output)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("setup_sects disagrees with packed decompressor offset", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_configured_physical_base_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, _image = fixture(root)
            config = build / ".config"
            base = int(config.read_text().splitlines()[0].partition("=")[2], 0)
            config.write_text(
                f"CONFIG_PHYSICAL_START=0x{base + 4096:x}\nCONFIG_PHYSICAL_ALIGN=0x1000\n"
            )
            output = root / "published"
            result = verify(build, output)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("physical load base differs from build config", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_stripped_loaded_source_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, _image = fixture(root)
            original = (build / "vmlinux").read_bytes()
            stripped = bytearray((root / "stripped-vmlinux").read_bytes())
            spec = importlib.util.spec_from_file_location("verify_x86_image", VERIFY)
            publisher = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(publisher)
            spec = importlib.util.spec_from_file_location("scan_x86_image", SCAN_UNCLAIMED)
            scanner = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(scanner)
            _programs, sections = scanner.headers(stripped)
            text_offset = next(offset for name, _kind, _flags, _address, offset, _size
                               in sections if name == ".text")
            stripped[text_offset] ^= 1
            with self.assertRaisesRegex(ValueError, "source bytes differ"):
                publisher.verify_runtime_kernel_identity(
                    original, bytes(stripped), (build / ".config").read_bytes(), scanner
                )

    def test_rejects_packed_image_unrelated_to_scanned_elfs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, image = fixture(root)
            (build / "arch/x86/boot/bzImage").write_bytes(image + b"different")
            output = root / "published"
            result = verify(build, output)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("packed bzImage differs", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_compressed_kernel_unrelated_to_scanned_elf(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, _image = fixture(root)
            source = root / "replacement.s"
            source.write_text('.section .text,"ax"\n.global _start\n_start:\nnop\nret\n')
            run("as", "--64", "-o", str(root / "replacement.o"), str(source))
            run("ld", "-nostdlib", "-T", str(root / "kernel.ld"), "-e", "_start",
                "-o", str(build / "vmlinux"),
                str(root / "replacement.o"))
            output = root / "published"
            result = verify(build, output)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("payload differs from scanned vmlinux", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_counter_opcode_before_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, _image = fixture(root)
            source = root / "counter.s"
            source.write_text('.section .text,"ax"\n.global _start\n_start:\nrdtsc\nret\n')
            run("as", "--64", "-o", str(root / "counter.o"), str(source))
            run("ld", "-nostdlib", "-T", str(root / "kernel.ld"), "-e", "_start",
                "-o", str(build / "vmlinux"),
                str(root / "counter.o"))
            output = root / "published"
            result = verify(build, output)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("counter-opcode scan rejected", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_unmapped_allocated_setup_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, _image = fixture(root)
            add_orphan_section(root, build / "arch/x86/boot/setup.elf", ".rodata", b"\x0f\x31")
            output = root / "published"
            result = verify(build, output)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("setup allocated section .rodata is not in one loaded PT_LOAD", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_unmapped_allocated_decompressor_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, _image = fixture(root)
            add_orphan_section(
                root, build / "arch/x86/boot/compressed/vmlinux", ".rodata", b"extra"
            )
            output = root / "published"
            result = verify(build, output)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("decompressor allocated section .rodata is not in one loaded PT_LOAD",
                          result.stderr)
            self.assertFalse(output.exists())

    def test_mutating_build_paths_after_snapshot_cannot_change_published_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build, image = fixture(root)
            kernel = (build / "vmlinux").read_bytes()
            boot_image = build / "arch/x86/boot/bzImage"
            replacement = root / "different-bzImage"
            replacement.write_bytes(image + b"different")
            wrapper = root / "bin"
            wrapper.mkdir()
            objdump = wrapper / "objdump"
            objdump.write_text(
                "#!/bin/sh\ncp \"$RACE_REPLACEMENT\" \"$RACE_IMAGE\"\n"
                "printf tampered > \"$RACE_KERNEL\"\n"
                "printf tampered > \"$RACE_SETUP\"\n"
                "printf tampered > \"$RACE_DECOMPRESSOR\"\n"
                "printf tampered > \"$RACE_CONFIG\"\n"
                "exec /usr/bin/objdump \"$@\"\n"
            )
            objdump.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = f"{wrapper}:{env['PATH']}"
            env["RACE_REPLACEMENT"] = str(replacement)
            env["RACE_IMAGE"] = str(boot_image)
            env["RACE_KERNEL"] = str(build / "vmlinux")
            env["RACE_SETUP"] = str(build / "arch/x86/boot/setup.elf")
            env["RACE_DECOMPRESSOR"] = str(build / "arch/x86/boot/compressed/vmlinux")
            env["RACE_CONFIG"] = str(build / ".config")
            output = root / "published"
            result = verify(build, output, env)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(boot_image.read_bytes(), replacement.read_bytes())
            self.assertEqual((output / "bzImage").read_bytes(), image)
            self.assertEqual((output / "x86_64/bzImage.vmlinux").read_bytes(), kernel)


if __name__ == "__main__":
    unittest.main()
