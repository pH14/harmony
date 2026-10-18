#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compile the production patch code with planted ingress and host failures.

These are boundary-mechanism tests, not a substitute for live kernel runs.
"""
import importlib.util
import os
import platform
from pathlib import Path
import re
import shutil
import shlex
import struct
import subprocess
import sys
import tempfile
import unittest

PATCHES = Path(__file__).resolve().parents[1] / "linux/patches"


def postimage(path, filename):
    result, active = [], False
    for line in path.read_text().splitlines():
        if line.startswith("+++ "):
            active = line == "+++ b/" + filename
        elif line.startswith("--- "):
            active = False
        elif active and line.startswith(("+", " ")):
            result.append(line[1:])
    return "\n".join(result)


def function(source, name):
    start = source.index(name + "(")
    start = source.index("{", start)
    depth = 1
    end = start + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


def syscall_capture_body():
    path = Path(__file__).with_name("n6-instruction-sweep.py")
    spec = importlib.util.spec_from_file_location("n6_sweep_boundary_test", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module.x86_body("SYSCALL R11 capture")


class KernelBoundaryContracts(unittest.TestCase):
    def test_af_primed_syscall_keeps_the_original_machine_code_size(self):
        body = syscall_capture_body()
        compiler = os.environ.get("CC", "cc")
        extra = []
        if platform.machine().lower() not in ("x86_64", "amd64"):
            compiler = shutil.which("clang")
            if compiler is None:
                self.skipTest("the non-x86 host needs clang's x86 ELF assembler")
            extra = ["--target=x86_64-linux-gnu"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / "probe.S", root / "probe.o"
            source.write_text(".text\n.global probe\nprobe:\n" + "\n".join(body) + "\n")
            subprocess.run([compiler, *extra, "-c", str(source), "-o", str(output)], check=True,
                           capture_output=True, text=True)
            elf = output.read_bytes()
            header = struct.unpack_from("<16sHHIQQQIHHHHHH", elf)
            sections = [struct.unpack_from("<IIQQQQIIQQ", elf, header[6] + index * header[11])
                        for index in range(header[12])]
            strings = sections[header[13]]
            names = elf[strings[4]:strings[4] + strings[5]]
            text = next(section for section in sections if names[section[0]:].split(b"\0", 1)[0] == b".text")
            actual = elf[text[4]:text[4] + text[5]]
        original = bytes.fromhex("b827000000 0f05 4c89d8 c3")
        self.assertEqual(actual, bytes.fromhex("31c0 ffc8 90 0f05 4c89d8 c3"))
        self.assertEqual(len(actual), len(original))
        self.assertNotIn(bytes.fromhex("0fc7"), actual)

    @unittest.skipUnless(platform.machine().lower() in ("x86_64", "amd64"), "executes native x86 flags")
    def test_syscall_primer_defines_af_and_an_invalid_syscall_number(self):
        primer = syscall_capture_body()
        primer = primer[:primer.index("syscall")]
        assembly = "\n\t".join(primer) + "\n\tpushfq\n\tpopq %1"
        assembly = assembly.replace("%eax", "%%eax")
        source = r'''#include <assert.h>
#include <stdint.h>
int main(void) {
 uint32_t number;
 unsigned long flags;
 __asm__ volatile (ASM : "=a"(number), "=r"(flags) : : "cc", "memory");
 assert(number == UINT32_MAX);
 assert((flags & 16UL) != 0);
 return 0;
}'''
        import json
        self.compile_run(source.replace("ASM", json.dumps(assembly)))

    def compile_run(self, source, args=(), expected=0):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            (base / "probe.c").write_text(source)
            subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Werror", "-Wall", "-Wextra", "-Wno-unused-parameter",
                            str(base / "probe.c"), "-o", str(base / "probe")], check=True, capture_output=True, text=True)
            result = subprocess.run([str(base / "probe"), *args], capture_output=True, text=True)
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)

    def test_failed_n6_audit_retains_the_rejected_binary_without_accepting_the_build(self):
        spec = importlib.util.spec_from_file_location("n6_entropy_fixture", Path(__file__).with_name("test_n6_entropy_scan.py"))
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        guest = PATCHES.parents[1]
        repository = guest.parents[1]
        builder = (guest / "linux/build-n6-instruction-images.sh").read_text()
        wrapper = (repository / "workloads/guest-images/nix/build-guest-images.sh").read_text()
        for text, expected in [(bytes.fromhex("0fc7f8c3"), 1), (b"\xc3", 0)]:
            with self.subTest(rejected=bool(expected)), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                work = root / "work"
                output = root / "output"
                n6 = work / "build/n6-instruction-sweep"
                n6.mkdir(parents=True)
                binary = n6 / "n6-instruction-guest"
                image = fixture.elf(62, text, b"\xc3", 6)
                binary.write_bytes(image)
                (n6 / "n6-generated.S").write_text("generated probe assembly")
                script = "set -euo pipefail\n" + "\n".join([
                    "work=" + shlex.quote(str(work)),
                    "output=" + shlex.quote(str(output)),
                    "n6=1",
                    "HARMONY_NIX_KEEP_WORK=0",
                    "n6_root=" + shlex.quote(str(n6)),
                    "n6_cc=" + shlex.quote(shutil.which("true")),
                    "entropy_scan=" + shlex.quote(str(guest / "scripts/n6-entropy-scan.py")),
                    "LINUX_DIR=" + shlex.quote(str(guest / "linux")),
                    "arch=x86_64",
                    "march=(-m64)",
                    "cleanup() " + function(wrapper, "cleanup"),
                    "build_guest() " + function(builder, "build_guest"),
                    "trap cleanup EXIT",
                    "build_guest traps-off " + shlex.quote(str(binary)),
                ])
                result = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                diagnostics = output / "diagnostics/n6"
                self.assertFalse(work.exists())
                if expected:
                    self.assertEqual((diagnostics / binary.name).read_bytes(), image)
                    self.assertEqual((diagnostics / "n6-generated.S").read_text(), "generated probe assembly")
                    self.assertIn("N6_ENTROPY_REJECT", (diagnostics / (binary.name + ".entropy-audit.txt")).read_text())
                else:
                    self.assertFalse(diagnostics.exists())

    def test_both_syscall_flag_copies_are_pinned_before_the_first_tick(self):
        patch = PATCHES / "x86/0001-x86-harmony-pvclock-exit-count-clocksource.patch"
        self.assertNotIn("regs->r11", postimage(patch, "kernel/entry/common.c"))
        self.assertEqual(patch.read_text().count("regs->r11 = harmony_pin_flags(regs->r11);"), 1)
        ingress = postimage(patch, "arch/x86/entry/syscall_64.c")
        start = ingress.index("regs->flags =")
        end = ingress.index("harmony_virtual_time_tick();", start) + len("harmony_virtual_time_tick();")
        ingress = ingress[start:end]
        source = '''#include <assert.h>
struct pt_regs { unsigned long flags, r11; };
static int pinned;
static struct pt_regs *current;
static unsigned long harmony_pin_flags(unsigned long flags) { return pinned ? flags & ~16UL : flags; }
static void harmony_virtual_time_tick(void) { assert(current->flags == current->r11); if(pinned) assert(!(current->r11 & 16)); }
static int syscall_ingress(struct pt_regs *regs) { BODY return regs->flags == regs->r11; }
int main(void) {
 for(pinned=0;pinned<2;pinned++) for(unsigned long af=0;af<=16;af+=16) {
  struct pt_regs regs={0x202|af,0x202|af}; current=&regs;
  assert(syscall_ingress(&regs)); assert(regs.r11==(pinned?0x202:0x202|af));
 }
 return 0;
}'''
        self.compile_run(source.replace("BODY", ingress))
        # The prior one-copy implementation must fail the same tick oracle.
        old = ingress.replace("regs->r11 = harmony_pin_flags(regs->r11);", "")
        with self.assertRaises(AssertionError):
            self.compile_run(source.replace("BODY", old))

    def test_uml_initial_xstate_does_not_import_pkru_or_extended_host_registers(self):
        patch = PATCHES / "um/0009-um-harmony-initial-fpu-state.patch"
        text = postimage(patch, "arch/um/os-Linux/start_up.c")
        block = text[text.index("\n\t\t{") + 1:text.index("#endif")]
        source = '''#include <assert.h>
#include <string.h>
#include <stddef.h>
int main(void) {
 unsigned char storage[4096]; void *exec_fp_regs=storage;
 for(size_t host_fp_size=512;host_fp_size<=sizeof(storage);host_fp_size+=64) {
  memset(storage,0xa5,sizeof(storage)); BODY
  assert(storage[0]==0x7f && storage[1]==3);
  assert(storage[24]==0x80 && storage[25]==0x1f);
  for(size_t i=32;i<464;i++) assert(storage[i]==0);
  for(size_t i=512;i<host_fp_size;i++) assert(storage[i]==0);
  for(size_t i=host_fp_size;i<sizeof(storage);i++) assert(storage[i]==0xa5);
 }
 return 0;
}'''
        self.compile_run(source.replace("BODY", block))

    def test_uml_substrate_failures_have_reserved_host_status_on_both_architectures(self):
        for series, filename in [("um", "0016-um-harmony-host-failure-provenance.patch"),
                                 ("um-arm64", "0021-um-harmony-host-failure-provenance.patch")]:
            with self.subTest(series=series):
                patch = PATCHES / series / filename
                helper = function(postimage(patch, "arch/um/os-Linux/util.c"), "os_host_failure")
                handler = function(postimage(patch, "arch/um/os-Linux/main.c"), "last_ditch_exit")
                process = postimage(patch, "arch/um/os-Linux/skas/process.c")
                # Compile each complete failure block from the patch, retaining
                # its real condition and terminating primitive.
                memory_start = process.index("if (stack == MAP_FAILED)")
                memory = process[memory_start:process.index("\n\t}", memory_start) + 3]
                socket_start = process.index("if (socketpair(")
                sockets = process[socket_start:process.index("\n\t}", socket_start) + 3]
                clone_start = process.index("if (mm_id->pid < 0)")
                clone = process[clone_start:process.index("\n\t}", clone_start) + 3]
                mmu = postimage(patch, "arch/um/kernel/skas/mmu.c")
                lost = re.search(r"if \(mm_context->id.pid == pid\)\s+os_host_failure\(\);", mmu)[0]
                source = '''#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <signal.h>
#include <errno.h>
#define HARMONY_UML_HOST_FAILURE_STATUS 87
#define MAP_FAILED ((void *)-1)
#define UM_KERN_ERR ""
#define printk(...) ((void)0)
#define AF_UNIX 1
#define SOCK_STREAM 1
struct context { struct { int pid; } id; };
struct mm { int pid; };
struct tramp { int sockpair[2]; };
static int fail;
static int socketpair(int family,int type,int protocol,int *fds) { (void)fds; return fail; }
static void os_host_failure(void) HELPER
static void last_ditch_exit(int sig) HANDLER
int main(int argc,char **argv) {
 if(argc!=2) return 3;
 fail=atoi(argv[1]);
 struct mm mm={fail?-1:1}; struct mm *mm_id=&mm;
 struct tramp tramp_data;
 void *stack=fail?MAP_FAILED:NULL;
 if(fail==1) { MEMORY }
 if(fail==2) { SOCKETS }
 if(fail==3) { CLONE }
 if(fail==4 || fail==5) {
  int sig=fail==4?SIGTERM:SIGINT; signal(sig,last_ditch_exit); raise(sig);
 }
 if(fail==6) { struct context context={{9}}; struct context *mm_context=&context; int pid=9; LOST }
 return 0;
}'''
                for key, value in [("HELPER", helper), ("HANDLER", handler), ("MEMORY", memory),
                                   ("SOCKETS", sockets), ("CLONE", clone), ("LOST", lost)]:
                    source = source.replace(key, value)
                self.compile_run(source, ("0",))
                for failure in range(1, 7):
                    self.compile_run(source, (str(failure),), expected=87)


if __name__ == "__main__":
    unittest.main()
