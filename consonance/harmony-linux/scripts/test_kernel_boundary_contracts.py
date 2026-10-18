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

    def test_flake_builders_retain_a_rejected_n6_guest_through_their_real_entrypoints(self):
        version = subprocess.run(["bash", "-c", 'echo "${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}"'],
                                 check=True, capture_output=True, text=True)
        if tuple(map(int, version.stdout.strip().split("."))) < (4, 4):
            self.skipTest("the locked Linux builders require Bash 4.4 or newer")
        spec = importlib.util.spec_from_file_location("n6_entropy_fixture", Path(__file__).with_name("test_n6_entropy_scan.py"))
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        guest = PATCHES.parents[1]
        repository = guest.parents[1]
        flake = (repository / "flake.nix").read_text()
        self.assertRegex(flake, r"guest-images\s*=\s*platformBuilder;")
        for binding, explicit_compiler in (("platformBuilder", False), ("workloadBuilder", False),
                                           ("platformBuilder", True), ("workloadBuilder", True)):
            entry = re.search(re.escape(binding) + r" = pkgs\.writeShellApplication \{.*?exec \$\{\./([^}]+)\}", flake, re.S)
            self.assertIsNotNone(entry)
            wrapper = repository / entry[1]
            unconditional = entry[0].split("${nixpkgs.lib.optionalString", 1)[0]
            source_exports = re.findall(r"export (HARMONY_NIX_(?:LINUX|BUSYBOX|MUSL)_SOURCE)=\$\{(\w+)\}", unconditional)
            self.assertEqual(dict(source_exports), {
                "HARMONY_NIX_LINUX_SOURCE": "linuxSource",
                "HARMONY_NIX_BUSYBOX_SOURCE": "busyboxSource",
                "HARMONY_NIX_MUSL_SOURCE": "muslSource",
            })
            with self.subTest(builder=binding, explicit_compiler=explicit_compiler), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / "source"
                linux = source / "consonance/harmony-linux/linux"
                scripts = source / "consonance/harmony-linux/scripts"
                linux.mkdir(parents=True)
                scripts.mkdir()
                for name in ("build-n6-instruction-images.sh",):
                    shutil.copyfile(guest / "linux" / name, linux / name)
                    (linux / name).chmod(0o755)
                for name in ("n6-instruction-sweep.py", "n6-entropy-scan.py", "instruction-contract.toml"):
                    shutil.copyfile(guest / "scripts" / name, scripts / name)
                (scripts / "runtime-artifacts.py").write_text('print("fixture-source-key")\n')
                (linux / "lib-build.sh").write_text('''BUILD_ROOT=$GUEST_BUILD_ROOT
ART_DIR=$HARMONY_ARTIFACT_DIR
ARM64_ART_DIR=$ART_DIR/aarch64
LINUX_DIR=$PWD
KSRC=$BUILD_ROOT/kernel-source
X86_MUSL_PREFIX=$BUILD_ROOT/musl-x86-prefix
require_tools() { for tool in "$@"; do command -v "$tool" >/dev/null; done; }
extract_kernel() { mkdir -p "$KSRC/usr"; }
build_x86_musl() {
    test -f "$HARMONY_DOWNLOAD_DIR/musl-1.2.6.tar.gz"
    cmp "$HARMONY_TEST_MUSL_TARBALL" "$HARMONY_DOWNLOAD_DIR/musl-1.2.6.tar.gz"
    mkdir -p "$X86_MUSL_PREFIX/bin"
    ln -s "$HARMONY_TEST_N6_CC" "$X86_MUSL_PREFIX/bin/musl-gcc"
    echo PINNED_X86_MUSL_SELECTED
}
''')
                for name in ("build-kernel.sh", "build-go-runtime-image.sh", "build-initramfs.sh"):
                    path = linux / name
                    path.write_text('''#!/usr/bin/env bash
if [ "${TASK_PARK_PROFILE:-0}" = 1 ]; then
    echo UNEXPECTED_POST_N6_BUILD
fi
exit 0
''')
                    path.chmod(0o755)
                tools = root / "tools"
                tools.mkdir()
                (tools / "uname").write_text('''#!/bin/sh
case "$1" in
    -s) echo Linux ;;
    -m) echo x86_64 ;;
    *) exec /usr/bin/uname "$@" ;;
esac
''')
                (tools / "uname").chmod(0o755)
                (tools / "nproc").write_text("#!/bin/sh\necho 4\n")
                (tools / "nproc").chmod(0o755)
                (tools / "cc").write_text("#!" + sys.executable + "\n" + '''import os
from pathlib import Path
import shutil
import sys
arguments = sys.argv[1:]
with (Path(os.environ["HARMONY_TEST_CC_FIXTURES"]) / "compiler-log").open("a") as log:
    log.write(Path(sys.argv[0]).name + "\\n")
output = Path(arguments[arguments.index("-o") + 1])
name = {"n6-entropy-negative": "negative", "n6-entropy-displacement": "displacement", "n6-instruction-guest": "rejected"}[output.name]
shutil.copyfile(Path(os.environ["HARMONY_TEST_CC_FIXTURES"]) / name, output)
''')
                (tools / "cc").chmod(0o755)
                fixtures = root / "compiler-fixtures"
                fixtures.mkdir()
                images = {
                    "negative": fixture.elf(62, bytes.fromhex("0fc7f0 0fc7f8 c3"), b"\xc3", 6),
                    "displacement": fixture.elf(62, bytes.fromhex("488d3d 0fc7f8 00 c3"), b"\xc3", 6),
                    "rejected": fixture.elf(62, bytes.fromhex("0fc7f8 c3"), b"\xc3", 6),
                }
                for name, image in images.items():
                    (fixtures / name).write_bytes(image)
                tarball = root / "source.tar"
                tarball.write_bytes(b"build-input-placeholder")
                temporary_work = root / "temporary-work"
                temporary_work.mkdir()
                output = root / "output"
                environment = os.environ.copy()
                environment.update({
                    "PATH": str(tools) + os.pathsep + environment.get("PATH", ""),
                    "TMPDIR": str(temporary_work),
                    "HARMONY_NIX_SOURCE": str(source),
                    "HARMONY_NIX_KEEP_WORK": "0",
                    "HARMONY_TEST_N6_CC": str(tools / "cc"),
                    "HARMONY_TEST_MUSL_TARBALL": str(tarball),
                    "HARMONY_TEST_CC_FIXTURES": str(fixtures),
                })
                for variable, source_binding in source_exports:
                    self.assertIn(source_binding, ("linuxSource", "busyboxSource", "muslSource"))
                    environment[variable] = str(tarball)
                environment.pop("HARMONY_N6_CC", None)
                if explicit_compiler:
                    environment["HARMONY_N6_CC"] = str(tools / "cc")
                result = subprocess.run(["bash", str(wrapper), "--output", str(output), "--n6"],
                                        cwd=root, env=environment, capture_output=True, text=True)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("ok: opcode-shaped LEA displacement rejected", result.stdout)
                self.assertIn("N6_ENTROPY_REJECT", result.stdout)
                self.assertIn("N6 failed-build diagnostics preserved:", result.stderr)
                self.assertNotIn("UNEXPECTED_POST_N6_BUILD", result.stdout)
                self.assertNotIn("PASS: Nix-locked", result.stdout)
                self.assertIn("PINNED_X86_MUSL_SELECTED", result.stdout,
                              f"compiler override={environment.get('HARMONY_N6_CC')!r}\n" + result.stdout + result.stderr)
                self.assertEqual((fixtures / "compiler-log").read_text().splitlines(), ["musl-gcc"] * 3)
                self.assertEqual(list(temporary_work.glob("harmony-nix-guest.*")), [])
                diagnostics = output / "diagnostics/n6"
                self.assertEqual((diagnostics / "n6-instruction-guest").read_bytes(), images["rejected"])
                self.assertIn("decl %eax", (diagnostics / "n6-generated.S").read_text())
                self.assertIn("N6_TABLE_ROW_COUNT", (diagnostics / "n6-generated.h").read_text())
                self.assertIn("N6_ENTROPY_REJECT", (diagnostics / "n6-instruction-guest.entropy-audit.txt").read_text())
                if explicit_compiler:
                    with self.subTest(entrypoint="standalone"):
                        (fixtures / "compiler-log").unlink()
                        standalone = root / "standalone-build"
                        environment["GUEST_BUILD_ROOT"] = str(standalone)
                        environment["HARMONY_ARTIFACT_DIR"] = str(root / "standalone-artifacts")
                        result = subprocess.run(["bash", str(linux / "build-n6-instruction-images.sh")],
                                                cwd=linux, env=environment, capture_output=True, text=True)
                        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                        self.assertIn("N6_ENTROPY_REJECT", result.stdout)
                        self.assertNotIn("PINNED_X86_MUSL_SELECTED", result.stdout)
                        self.assertEqual((fixtures / "compiler-log").read_text().splitlines(), ["cc"] * 3)
                        self.assertEqual((standalone / "n6-instruction-sweep/n6-instruction-guest").read_bytes(),
                                         images["rejected"])

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

    def test_uml_checkpoint_host_checkpoint_failures_never_become_guest_panics(self):
        for series, provenance in [("um", "0016-um-harmony-host-failure-provenance.patch"),
                                   ("um-arm64", "0021-um-harmony-host-failure-provenance.patch")]:
            with self.subTest(series=series):
                checkpoint = postimage(PATCHES / series / "0008-um-harmony-checkpoint.patch",
                                       "arch/um/os-Linux/skas/checkpoint.c")
                primitive = postimage(PATCHES / series / provenance, "arch/um/os-Linux/util.c")
                signatures = {
                    "os_host_failure": "static void os_host_failure(void) __attribute__((noreturn));\nstatic void os_host_failure(void)",
                    "page_size": "static unsigned long page_size(void)",
                    "round_up": "static unsigned long round_up(unsigned long value, unsigned long page)",
                    "checkpoint_failure": "static void checkpoint_failure(const char *operation)",
                    "os_track_init": "void os_track_init(unsigned long base, unsigned long start, unsigned long end)",
                    "set_page": "static void set_page(unsigned long pfn)",
                    "os_track_mark": "void os_track_mark(unsigned long long phys, unsigned long length)",
                    "open_all": "static void open_all(void)",
                    "open_range": "static void open_range(unsigned long start, unsigned long end, const char *operation)",
                    "os_track_fault": "int os_track_fault(unsigned long address)",
                    "protect_all": "static void protect_all(void)",
                    "fatal": "static void fatal(const char *what, int error)",
                }
                source = r'''#define _GNU_SOURCE
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/mman.h>
#define UM_KERN_PAGE_SHIFT 12
#define UM_KERN_PAGE_SIZE (1UL << UM_KERN_PAGE_SHIFT)
#define UM_KERN_PAGE_MASK (~(UM_KERN_PAGE_SIZE - 1))
#define LONG_BITS (8 * sizeof(unsigned long))
#define UNPROTECT_LIMIT 16384
#define HARMONY_UML_HOST_FAILURE_STATUS 87
struct track { unsigned long base,start,end,pages,words,*bits,unprotects; int tracking,all; };
static struct track track;
static int failure, calls;
static unsigned long bits[1];
static void *injected_mmap(void *address, size_t length, int prot, int flags, int fd, off_t offset) {
 errno=ENOMEM; return failure==1 ? MAP_FAILED : bits;
}
static int injected_mprotect(void *address, size_t length, int prot) {
 assert(length>0); calls++; errno=ENOMEM; return failure ? -1 : 0;
}
#define mmap injected_mmap
#define mprotect injected_mprotect
FUNCTIONS
int main(int argc, char **argv) {
 assert(argc==2); failure=atoi(argv[1]);
 os_track_init(0x1000,0x2000,0x4000);
 assert(track.pages==3 && track.words==1 && track.bits==bits);
 track.tracking=1;
 if(failure==4 || !failure) protect_all();
 if(failure==2 || !failure) {
  assert(os_track_fault(0x1000)==0); assert(os_track_fault(0x4000)==0);
  assert(os_track_fault(0x2001)==1); assert(bits[0]==2);
 }
 if(failure==3 || !failure) {
  track.unprotects=UNPROTECT_LIMIT; assert(os_track_fault(0x3001)==1); assert(track.all);
 }
 if(failure==5) fatal("injected restore operation",ENOMEM);
 assert(!failure && calls==3); return 0;
}
'''
                functions = "\n".join(signature + "\n" + function(primitive if name == "os_host_failure" else checkpoint, name)
                                      for name, signature in signatures.items())
                source = source.replace("FUNCTIONS", functions)
                self.compile_run(source, ("0",))
                for failure in range(1, 6):
                    self.compile_run(source, (str(failure),), expected=87)
                # A planted panic classification must fail the same exit oracle.
                old = source.replace("os_host_failure();", "_exit(86);", 1)
                with self.assertRaises(AssertionError):
                    self.compile_run(old, ("2",), expected=87)
                self.assertNotIn("RESTORE_FAILED", checkpoint)
                mmu = postimage(PATCHES / series / "0008-um-harmony-checkpoint.patch",
                                "arch/um/kernel/skas/mmu.c")
                rebuild = function(mmu, "um_checkpoint_rebuild_stubs")
                self.assertNotIn("panic(", rebuild)
                self.assertIn("os_host_failure();", rebuild)

    def test_uml_checkpoint_rebuild_and_irq_restore_fail_closed(self):
        for series, provenance in [("um", "0016-um-harmony-host-failure-provenance.patch"),
                                   ("um-arm64", "0021-um-harmony-host-failure-provenance.patch")]:
            with self.subTest(series=series):
                patch = PATCHES / series / "0008-um-harmony-checkpoint.patch"
                primitive = function(postimage(PATCHES / series / provenance,
                                               "arch/um/os-Linux/util.c"), "os_host_failure")
                rebuild = function(postimage(patch, "arch/um/kernel/skas/mmu.c"),
                                   "um_checkpoint_rebuild_stubs")
                failure = re.search(r"if \(ret < 0\) \{.*?\n\t\t\}", rebuild, re.S)[0]
                source = r'''#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#define KERN_ERR ""
#define printk printf
#define HARMONY_UML_HOST_FAILURE_STATUS 87
static void os_host_failure(void) HELPER
int main(int argc, char **argv) {
 if(argc!=2) return 99;
 int ret=atoi(argv[1]); FAILURE_BLOCK return 0;
}
'''
                source = source.replace("HELPER", primitive).replace("FAILURE_BLOCK", failure)
                self.compile_run(source, ("0",))
                self.compile_run(source, ("-12",), expected=87)

                irq = postimage(patch, "arch/um/kernel/irq.c")
                source = r'''#include <assert.h>
#include <stdbool.h>
#include <stdlib.h>
#include <unistd.h>
#define HARMONY_UML_HOST_FAILURE_STATUS 87
#define NUM_IRQ_TYPES 3
enum um_irq_type { READ, WRITE, OTHER };
struct reg { int events; };
struct irq_entry { struct irq_entry *next; int fd; struct reg reg[NUM_IRQ_TYPES]; bool epoll_registered; };
static struct irq_entry entries[] = {
 { .next=&entries[1], .fd=10, .reg={{1},{4},{16}}, .epoll_registered=true },
 { .next=&entries[2], .fd=20, .reg={{2},{8},{32}}, .epoll_registered=true },
 { .next=&entries[3], .fd=30, .reg={{0},{0},{0}} },
 { .next=NULL, .fd=40, .reg={{1},{4},{16}} },
};
static struct irq_entry *active_fds=entries;
#define list_for_each_entry(entry, head, member) for(entry=*(head);entry;entry=entry->next)
static int failure, async_calls, adds, deletes;
static int os_set_fd_async(int fd) { async_calls++; return failure==1 && fd==20 ? -1 : 0; }
static int os_add_epoll_fd(int events, int fd, void *data) {
 struct irq_entry *entry=data;
 assert(entry->fd==fd);
 assert(events==(fd==10 ? 21 : 42)); adds++;
 return (failure==2 && fd==20) || ((failure==3 || failure==6) && fd==10) ? -1 : 0;
}
static int os_checkpoint_delete_irq(int fd) { assert(fd==30); deletes++; return failure==4 ? -1 : 0; }
static void os_host_failure(void) HELPER
static bool update_irq_entry_result(struct irq_entry *entry, int *result) RESULT
static bool update_irq_entry(struct irq_entry *entry) WRAPPER
void um_irq_checkpoint_attach(void) ATTACH
int main(int argc, char **argv) {
 assert(argc==2); failure=atoi(argv[1]);
 if(failure==6) {
  assert(update_irq_entry(&entries[0]));
  assert(entries[0].epoll_registered);
  return 0;
 }
 if(failure==4 || failure==5) {
  entries[2].epoll_registered=true;
  assert(!update_irq_entry(&entries[2]));
  assert(!entries[2].epoll_registered && deletes==1);
  return 0;
 }
 if(failure==3) {
  entries[0].epoll_registered=false;
  assert(update_irq_entry(&entries[0]));
  assert(!entries[0].epoll_registered);
  um_irq_checkpoint_attach();
  assert(async_calls==4 && adds==2 && deletes==0);
  return 0;
 }
 um_irq_checkpoint_attach();
 if(failure) return 0;
 assert(async_calls==4 && adds==2 && deletes==0);
 assert(update_irq_entry(&entries[0]));
 assert(entries[0].epoll_registered);
 assert(!update_irq_entry(&entries[2]));
 assert(!entries[2].epoll_registered && deletes==0);
 return 0;
}
'''
                for key, value in [("HELPER", primitive),
                                   ("RESULT", function(irq, "update_irq_entry_result")),
                                   ("WRAPPER", function(irq, "update_irq_entry")),
                                   ("ATTACH", function(irq, "um_irq_checkpoint_attach"))]:
                    source = source.replace(key, value)
                self.compile_run(source, ("0",))
                self.compile_run(source, ("3",))
                self.compile_run(source, ("5",))
                self.compile_run(source, ("6",))
                for failure in (1, 2, 4):
                    self.compile_run(source, (str(failure),), expected=87)
                # Removing either checkpoint check must permit the bad restore
                # and fail the reserved infrastructure-exit oracle.
                for condition, failure in [("if (os_set_fd_async(entry->fd) < 0)", 1),
                                           ("if (result < 0)", 2)]:
                    old = source.replace(condition + "\n\t\t\tos_host_failure();",
                                         condition + "\n\t\t\t(void)0;")
                    self.assertNotEqual(old, source)
                    with self.assertRaises(AssertionError):
                        self.compile_run(old, (str(failure),), expected=87)

    def test_uml_checkpoint_reconstruction_checks_host_calls_and_expected_absence(self):
        for series, provenance in [("um", "0016-um-harmony-host-failure-provenance.patch"),
                                   ("um-arm64", "0021-um-harmony-host-failure-provenance.patch")]:
            with self.subTest(series=series):
                patch = PATCHES / series / "0008-um-harmony-checkpoint.patch"
                checkpoint = postimage(patch, "arch/um/os-Linux/skas/checkpoint.c")
                primitive = function(postimage(PATCHES / series / provenance,
                                               "arch/um/os-Linux/util.c"), "os_host_failure")
                source = r'''#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
#ifndef __WALL
#define __WALL 0x40000000
#endif
#define HARMONY_UML_HOST_FAILURE_STATUS 87
static int mode, closes, waits, signals;
static int injected_close(int fd) {
 assert(fd==7); closes++;
 if(mode==1 || mode==2) { errno=mode==1?EPERM:EINTR; return -1; }
 return 0;
}
static int os_del_epoll_fd(int fd) {
 assert(fd==7 && mode!=13);
 if(mode==3 || mode==4) { errno=mode==3?EPERM:ENOENT; return -1; }
 return 0;
}
static int injected_sigprocmask(int how,const sigset_t *set,sigset_t *old) {
 assert(how==SIG_BLOCK || how==SIG_SETMASK); assert(set);
 if(mode==5) { errno=EPERM; return -1; } return 0;
}
static int injected_sigtimedwait(const sigset_t *set,void *info,const struct timespec *zero) {
 assert(sigismember(set,SIGCHLD)==1); assert(!zero->tv_sec && !zero->tv_nsec); signals++;
 if(mode==6) { errno=EPERM; return -1; }
 if(mode==7 && signals==1) { errno=EINTR; return -1; }
 if(mode==7 && signals==2) return SIGCHLD;
 errno=EAGAIN; return -1;
}
static int injected_kill(int pid,int signal) {
 assert(pid==7 && signal==SIGKILL);
 if(mode==8 || mode==9) { errno=mode==8?EPERM:ESRCH; return -1; }
 return 0;
}
static pid_t injected_waitpid(int pid,int *status,int flags) {
 assert(pid==7 && flags==__WALL); waits++;
 if(mode==10) { errno=ECHILD; return -1; }
 if(mode==11 && waits==1) { errno=EINTR; return -1; }
 return mode==12 ? 8 : pid;
}
#define close injected_close
#define sigprocmask injected_sigprocmask
#define sigtimedwait injected_sigtimedwait
#define kill injected_kill
#define waitpid injected_waitpid
struct irq_entry { struct irq_entry *next; int fd,epoll_registered; };
static struct irq_entry entry={NULL,7,1};
static struct irq_entry *active_fds=&entry;
#define list_for_each_entry(entry, head, member) for(entry=*(head);entry;entry=entry->next)
static void os_host_failure(void) __attribute__((noreturn));
static void os_host_failure(void) PRIMITIVE
static void checkpoint_failure(const char *operation) FAILURE_HELPER
void os_checkpoint_close_fd(int fd) CLOSE_HELPER
int os_checkpoint_delete_irq(int fd) DELETE_HELPER
static void checkpoint_signal_mask(int how,const sigset_t *set,sigset_t *old) MASK_HELPER
static void drain_child_signals(void) DRAIN_HELPER
void os_checkpoint_kill(int pid) KILL_HELPER
void um_irq_checkpoint_detach(void) DETACH_HELPER
int main(int argc,char **argv) {
 assert(argc==2); mode=atoi(argv[1]);
 if(mode==13) { entry.epoll_registered=0; um_irq_checkpoint_detach(); return 0; }
 if(mode<=2) { errno=EACCES; os_checkpoint_close_fd(7); assert(errno==EACCES && closes==1); }
 if(mode==0 || mode==3 || mode==4) um_irq_checkpoint_detach();
 if(mode==0 || mode==5) {
  sigset_t set,old; sigfillset(&set);
  checkpoint_signal_mask(SIG_BLOCK,&set,&old); checkpoint_signal_mask(SIG_SETMASK,&set,NULL);
 }
 if(mode==0 || mode==6 || mode==7) {
  drain_child_signals(); assert(signals==(mode==7?3:1));
 }
 if(mode==0 || mode>=8) {
  os_checkpoint_kill(0); assert(waits==0); os_checkpoint_kill(7); assert(waits==(mode==11?2:1));
 }
 return 0;
}
'''
                helpers = [("PRIMITIVE", primitive),
                           ("DETACH_HELPER", function(postimage(patch, "arch/um/kernel/irq.c"), "um_irq_checkpoint_detach"))]
                for key, name in [("FAILURE_HELPER", "checkpoint_failure"),
                                  ("CLOSE_HELPER", "os_checkpoint_close_fd"),
                                  ("DELETE_HELPER", "os_checkpoint_delete_irq"),
                                  ("MASK_HELPER", "checkpoint_signal_mask"),
                                  ("DRAIN_HELPER", "drain_child_signals"),
                                  ("KILL_HELPER", "os_checkpoint_kill")]:
                    helpers.append((key, function(checkpoint, name)))
                for key, value in helpers:
                    source = source.replace(key, value)
                for mode in (0, 2, 4, 7, 9, 11, 13):
                    self.compile_run(source, (str(mode),))
                for mode in (1, 3, 5, 6, 8, 10, 12):
                    self.compile_run(source, (str(mode),), expected=87)
                for operation, mode in [("close host descriptor", 1), ("set host signal mask", 5),
                                        ("drain host child signals", 6), ("kill host stub", 8),
                                        ("reap host stub", 10)]:
                    old = source.replace('checkpoint_failure("' + operation + '");', '(void)0;')
                    self.assertNotEqual(old, source)
                    with self.assertRaises(AssertionError):
                        self.compile_run(old, (str(mode),), expected=87)
                old = source.replace("if (entry->epoll_registered &&\n\t\t    os_checkpoint_delete_irq(entry->fd) < 0)\n\t\t\tos_host_failure();",
                                     "(void)os_checkpoint_delete_irq(entry->fd);")
                self.assertNotEqual(old, source)
                with self.assertRaises(AssertionError):
                    self.compile_run(old, ("3",), expected=87)

    def test_uml_resumed_capture_does_not_close_its_stale_image_descriptor(self):
        for series in ("um", "um-arm64"):
            with self.subTest(series=series):
                source = r'''#include <assert.h>
#include <fcntl.h>
#include <stdlib.h>
#include <unistd.h>
#define UM_CONTROL_CAPTURE 1
#define UM_CONTROL_RESTORE 2
#define MAX_STUB_FDS 2
struct um_checkpoint_args { long result; };
static int mode,closes,saves,restores,callbacks,opened,rebuilt,attached;
static int stub_fds[MAX_STUB_FDS],um_checkpoint_resumed;
static void replace_descriptor(const char *name) {
 int file=open(name,O_RDONLY); assert(file>=0);
 if(file!=7) { assert(dup2(file,7)==7); assert(close(file)==0); }
}
static int um_checkpoint_stub_fds(int *fds,int capacity) {
 assert(fds==stub_fds && capacity==MAX_STUB_FDS); return mode==3?-12:1;
}
static void os_checkpoint_close_fd(int fd) { assert(fd==7); closes++; assert(close(fd)==0); }
static void fill_args(struct um_checkpoint_args *args,int fd,unsigned long long flags,int count) {
 assert(fd==7 && flags==0 && count==1); args->result=0;
}
#define local_irq_save(irq) do { (irq)=3; saves++; } while(0)
#define local_irq_restore(irq) do { assert((irq)==3); restores++; } while(0)
static void mark_untracked(void) {}
static void os_checkpoint_capture(void *args) {}
static void initial_thread_cb(void (*callback)(void *),struct um_checkpoint_args *args) {
 assert(callback==os_checkpoint_capture); callbacks++;
 args->result=mode==2?-12:0;
 if(mode==1) {
  assert(close(7)==0);
  replace_descriptor("/dev/zero");
  um_checkpoint_resumed=1;
 }
}
static void open_stacks(void) { opened++; }
static void um_checkpoint_rebuild_stubs(void) { assert(!um_checkpoint_resumed); rebuilt++; }
static void um_irq_checkpoint_attach(void) { attached++; }
static long capture(unsigned long long flags,int fd,unsigned int *ack) CAPTURE_BODY
int main(int argc,char **argv) {
 assert(argc==2); mode=atoi(argv[1]); replace_descriptor("/dev/null");
 unsigned int ack=0; long result=capture(0,7,&ack);
 assert(result==((mode==2 || mode==3)?-12:0));
 assert(closes==(mode==1?0:1));
 assert(saves==(mode==3?0:1) && restores==saves && callbacks==saves && opened==saves);
 assert(rebuilt==(mode==1) && attached==rebuilt);
 assert(ack==(mode==1?UM_CONTROL_RESTORE:UM_CONTROL_CAPTURE));
 if(mode==1) {
  char byte=1; assert(fcntl(7,F_GETFD)>=0); assert(read(7,&byte,1)==1 && byte==0); assert(close(7)==0);
 }
 return 0;
}
'''
                body = function(postimage(PATCHES / series / "0008-um-harmony-checkpoint.patch",
                                          "arch/um/kernel/skas/checkpoint.c"), "capture")
                current = source.replace("CAPTURE_BODY", body)
                for mode in range(4):
                    self.compile_run(current, (str(mode),))
                stale = body.replace("\n\tos_checkpoint_close_fd(fd);\n\tlocal_irq_restore(irq);\n\treturn args.result;",
                                     "\n\tlocal_irq_restore(irq);\n\treturn args.result;")
                stale = stale.replace("\topen_stacks();", "\tos_checkpoint_close_fd(fd);\n\topen_stacks();")
                self.assertNotEqual(stale, body)
                with self.assertRaises(AssertionError):
                    self.compile_run(source.replace("CAPTURE_BODY", stale), ("1",))

    def test_uml_checkpoint_checks_x86_tls_heap_and_boot_restore(self):
        for series, provenance in [("um", "0016-um-harmony-host-failure-provenance.patch"),
                                   ("um-arm64", "0021-um-harmony-host-failure-provenance.patch")]:
            with self.subTest(series=series):
                patch = PATCHES / series / "0008-um-harmony-checkpoint.patch"
                checkpoint = postimage(patch, "arch/um/os-Linux/skas/checkpoint.c")
                kernel = postimage(patch, "arch/um/kernel/skas/checkpoint.c")
                primitive = function(postimage(PATCHES / series / provenance,
                                               "arch/um/os-Linux/util.c"), "os_host_failure")
                source = r'''#include <assert.h>
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#define HARMONY_UML_HOST_FAILURE_STATUS 87
#define SYS_arch_prctl 158
#define SYS_brk 12
#define ARCH_GET_FS 0x1003
#define ARCH_SET_FS 0x1002
#define KERN_ERR ""
#define printk printf
enum { KIND_PRIVATE=1, KIND_HEAP, KIND_RESERVE };
struct image_region { unsigned long start,end,offset; unsigned int prot,kind; };
struct mapping { unsigned long start,end; };
static int mode,brks,mapped,restore_at_boot;
static long injected_syscall(long number,...) {
 va_list ap; va_start(ap,number);
 if(number==SYS_arch_prctl) {
  int operation=va_arg(ap,int);
  if(operation==ARCH_GET_FS) {
   unsigned long *tls=va_arg(ap,unsigned long *); *tls=0x1234; va_end(ap);
   return mode==1 ? -1 : 0;
  }
  assert(operation==ARCH_SET_FS); assert(va_arg(ap,unsigned long)==0x1234); va_end(ap);
  return mode==2 ? -1 : 0;
 }
 assert(number==SYS_brk); unsigned long end=va_arg(ap,unsigned long); va_end(ap); brks++;
 return mode==3 ? 0x3000 : mode==4 ? -1 : (long)end;
}
#define syscall injected_syscall
static int os_harmony_available(void) { return mode!=6; }
static unsigned long long time_travel_read(void) { return 7; }
static int os_harmony_exchange(unsigned long long moment,const void *request,unsigned long length,void *response,unsigned long capacity) {
 assert(moment==7 && !request && !length && !response && !capacity); return mode==7?-EPIPE:0;
}
static void ensure_mapped(unsigned long start,unsigned long end,int prot) {
 assert(start==0x2000 && end==0x4000 && prot==(PROT_READ|PROT_WRITE)); mapped++;
}
static int read_all(int fd,void *data,unsigned long length,unsigned long offset) { return 0; }
static void fatal(const char *what,int error) __attribute__((noreturn));
static void fatal(const char *what,int error) { _exit(87); }
static void os_host_failure(void) __attribute__((noreturn));
static void os_host_failure(void) PRIMITIVE
static void checkpoint_failure(const char *operation) FAILURE_HELPER
static unsigned long get_tls(void) GET_HELPER
static void set_tls(unsigned long tls) SET_HELPER
static void restore_region(int image,const struct image_region *region,const struct mapping *heap) REGION_HELPER
static int harmony_restore_at_boot(void) BOOT_HELPER
int main(int argc,char **argv) {
 assert(argc==2); mode=atoi(argv[1]);
 if(mode<=2) { assert(get_tls()==0x1234); set_tls(0x1234); }
 if(mode==0 || (mode>=3 && mode<=5)) {
  struct image_region region={.start=0x2000,.end=0x4000,.prot=PROT_READ|PROT_WRITE,.kind=KIND_HEAP};
  struct mapping heap={.start=0x2000,.end=mode==5?0x5000:0x3000};
  restore_region(7,&region,&heap); assert(mapped==1 && brks==(mode==5?0:1));
 }
 if(mode>=6) { restore_at_boot=mode!=9; assert(harmony_restore_at_boot()==0); }
 return 0;
}
'''
                for key, value in [("PRIMITIVE", primitive),
                                   ("FAILURE_HELPER", function(checkpoint, "checkpoint_failure")),
                                   ("GET_HELPER", function(checkpoint, "get_tls")),
                                   ("SET_HELPER", function(checkpoint, "set_tls")),
                                   ("REGION_HELPER", function(checkpoint, "restore_region")),
                                   ("BOOT_HELPER", function(kernel, "harmony_restore_at_boot"))]:
                    source = source.replace(key, value)
                for mode in (0, 5, 9):
                    self.compile_run(source, (str(mode),))
                for mode in (1, 2, 3, 4, 6, 7, 8):
                    self.compile_run(source, (str(mode),), expected=87)
                for operation, mode in [("read host thread pointer", 1),
                                        ("restore host thread pointer", 2), ("extend host heap", 3)]:
                    old = source.replace('checkpoint_failure("' + operation + '");', '(void)0;')
                    self.assertNotEqual(old, source)
                    with self.assertRaises(AssertionError):
                        self.compile_run(old, (str(mode),), expected=87)

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
