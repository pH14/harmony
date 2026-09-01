# SPDX-License-Identifier: AGPL-3.0-or-later
{
  description = "Locked, offline Harmony guest-image builder";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-linux" "x86_64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          isArm64 = system == "aarch64-linux";
          agentCargoVendor = pkgs.rustPlatform.importCargoLock {
            lockFile = ./consonance/harmony-linux/tetanes-agent/Cargo.lock;
          };
          rustCargoVendor = pkgs.rustPlatform.fetchCargoVendor {
            name = "harmony-rust-std-cargo-vendor";
            src = pkgs.rustPlatform.rustLibSrc;
            hash = "sha256-5oJ/mtsJW0R3F7jgxafP23+WMLkyMKu10De5WIzb7Ro=";
          };
          cargoVendor = pkgs.symlinkJoin {
            name = "harmony-guest-cargo-vendor";
            # importCargoLock is already a flat Cargo directory; the newer
            # fetchCargoVendor helper keeps registry crates one level below
            # its metadata root. Join that registry directory, not the root.
            paths = [ agentCargoVendor "${rustCargoVendor}/source-registry-0" ];
          };
          linuxSource = pkgs.fetchurl {
            url = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.35.tar.xz";
            sha256 = "f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236";
          };
          busyboxSource = pkgs.fetchurl {
            url = "https://busybox.net/downloads/busybox-1.38.0.tar.bz2";
            sha256 = "34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2";
          };
          muslSource = pkgs.fetchurl {
            url = "https://musl.libc.org/releases/musl-1.2.6.tar.gz";
            sha256 = "d585fd3b613c66151fc3249e8ed44f77020cb5e6c1e635a616d3f9f82460512a";
          };
          postgresSource = pkgs.fetchurl {
            url = "https://ftp.postgresql.org/pub/source/v17.10/postgresql-17.10.tar.bz2";
            sha256 = "078a03516dcdbdb705fecaf415ea3d13a956c589e46f09fed68a06fb00598c90";
          };
          rustSourceRoot = pkgs.rustPlatform.rustLibSrc;
          # Rust's musl target needs the platform unwind ABI while rebuilding
          # std. GCC's pinned musl runtime supplies that ABI in libgcc_eh.a;
          # using it avoids pulling an entire native LLVM/Clang toolchain into
          # the guest-image closure merely to obtain the unwind symbols.
          rustMuslUnwind = pkgs.runCommand "harmony-rust-musl-libunwind.a" {
            nativeBuildInputs = [ pkgs.stdenv.cc pkgs.binutils ];
          } ''
            archive=$(
              find ${pkgs.pkgsMusl.stdenv.cc.cc}/lib/gcc \
                -name libgcc_eh.a -print -quit
            )
            test -n "$archive"
            cp "$archive" "$out"
            chmod u+w "$out"
            cc -c -march=armv8.1-a+lse \
              ${./consonance/harmony-linux/nix/aarch64-lse-unwind-atomics.S} \
              -o lse-unwind-atomics.o
            ar r "$out" lse-unwind-atomics.o
            ranlib "$out"
          '';
          builder = pkgs.writeShellApplication {
            name = "harmony-build-guest-images";
            runtimeInputs = with pkgs; [
              bash
              bc
              binutils
              bison
              bzip2
              cargo
              coreutils
              cpio
              diffutils
              file
              findutils
              flex
              gawk
              gnumake
              gnugrep
              gnused
              gnutar
              gzip
              patch
              perl
              python3
              util-linux
              which
              xz
            ] ++ nixpkgs.lib.optionals isArm64 [
              cargo
              gcc
              rustc
            ] ++ nixpkgs.lib.optionals (!isArm64) [
              elfutils
              elfutils.dev
              gcc13
              glibc.static
              openssl
              openssl.dev
              pkg-config
            ];
            text = ''
              export HARMONY_NIX_SOURCE=${self.outPath}
              export HARMONY_NIX_LINUX_SOURCE=${linuxSource}
              export HARMONY_NIX_BUSYBOX_SOURCE=${busyboxSource}
              export HARMONY_NIX_SMB_SHA256=0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea
              ${nixpkgs.lib.optionalString isArm64 ''
                export HARMONY_NIX_MUSL_SOURCE=${muslSource}
                export HARMONY_NIX_POSTGRES_SOURCE=${postgresSource}
                export HARMONY_NIX_CARGO_VENDOR=${cargoVendor}
                export HARMONY_NIX_RUST_SOURCE_ROOT=${rustSourceRoot}
                export HARMONY_NIX_RUST_LIBUNWIND=${rustMuslUnwind}
              ''}
              ${nixpkgs.lib.optionalString (!isArm64) ''
                export NIX_CFLAGS_COMPILE="-I${pkgs.elfutils.dev}/include -I${pkgs.openssl.dev}/include''${NIX_CFLAGS_COMPILE:+ $NIX_CFLAGS_COMPILE}"
                export NIX_LDFLAGS="-L${pkgs.elfutils.out}/lib -L${pkgs.openssl.out}/lib -L${pkgs.glibc.static}/lib''${NIX_LDFLAGS:+ $NIX_LDFLAGS}"
                export LIBRARY_PATH="${pkgs.glibc.static}/lib''${LIBRARY_PATH:+:$LIBRARY_PATH}"
                export PKG_CONFIG_PATH="${pkgs.elfutils.dev}/lib/pkgconfig:${pkgs.openssl.dev}/lib/pkgconfig''${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
                export HARMONY_RDTSC_ALLOWLIST="$HARMONY_NIX_SOURCE/consonance/harmony-linux/linux/rdtsc-allowlist-gha.txt"
                export HARMONY_RDRAND_ALLOWLIST="$HARMONY_NIX_SOURCE/consonance/harmony-linux/linux/rdrand-allowlist-gha.txt"
              ''}
              exec ${./consonance/harmony-linux/nix/build-guest-images.sh} "$@"
            '';
          };
        in {
          guest-images = builder;
          default = builder;
        });

      apps = forAllSystems (system: {
        guest-images = {
          type = "app";
          program = "${self.packages.${system}.guest-images}/bin/harmony-build-guest-images";
        };
        default = self.apps.${system}.guest-images;
      });
    };
}
