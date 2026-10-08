# Runtime notices and corresponding source

Harmony and this frontend are AGPL-3.0-or-later (see LICENSE). The exact runtime
inputs and SHA-256 digests are in runtime-lock.json. The CLI, UML patches,
workload and assembly scripts are in the corresponding source archive:
https://github.com/pH14/harmony/releases/download/raft-browser-runtime-v2/harmony-raft-runtime-source.tar.gz

Third-party components retain their licenses:

- QEMU-Wasm (QEMU 8.2.0, GPL-2.0 and component-specific licenses):
  https://github.com/ktock/qemu-wasm and https://github.com/ktock/qemu-wasm-demo.
  Tested amd64-alpine distribution: image revision bfd3583840ae77706c179e00905a121b24c59a2f;
  kernel/ROM distribution: b7c549b5e6f4c376f76483a03e983214421434ad at
  https://github.com/ktock/qemu-wasm-demo-images. These are upstream binaries;
  no byte-identical local QEMU rebuild is claimed.
- Linux (GPL-2.0): outer Alpine kernel 6.12.43; inner UML source/version pinned
  in Harmony's linux build scripts. https://www.kernel.org and
  https://gitlab.alpinelinux.org/alpine/aports.
- BusyBox 1.38.0 (GPL-2.0): https://busybox.net/downloads/busybox-1.38.0.tar.bz2;
  configuration in Harmony tools/build-busybox.sh.
- GNU tar, glibc, GCC runtime: https://www.gnu.org/software/tar/,
  https://www.gnu.org/software/libc/, https://gcc.gnu.org/.
  Library copyright notices are included inside the outer image.
- Debian libraries and dash retain their copyright notices. The workload's
  pinned base and packages are described by workload/Dockerfile in the source.
- xterm.js 5.5.0 (MIT): vendor/xterm-LICENSE.
- xterm-pty 0.12.0 (MIT): vendor/xterm-pty-LICENSE.

The VM has no network device. The site fetches static files; machine state
and execution remain in the visitor's browser.
