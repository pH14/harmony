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
          linuxSource = pkgs.fetchurl {
            url = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.35.tar.xz";
            sha256 = "f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236";
          };
          busyboxSource = pkgs.fetchurl {
            urls = [
              "https://sources.buildroot.net/busybox/busybox-1.38.0.tar.bz2"
              "https://busybox.net/downloads/busybox-1.38.0.tar.bz2"
            ];
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
          builder = pkgs.writeShellApplication {
            name = "harmony-build-guest-images";
            runtimeInputs = with pkgs; [
              bash
              bc
              binutils
              bison
              bzip2
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
              gcc
            ] ++ nixpkgs.lib.optionals (!isArm64) [
              elfutils
              elfutils.dev
              gcc13
              go
              glibc.static
              openssl
              openssl.dev
              pkg-config
            ];
            text = ''
              export HARMONY_NIX_SOURCE=${self.outPath}
              export HARMONY_NIX_LINUX_SOURCE=${linuxSource}
              export HARMONY_NIX_BUSYBOX_SOURCE=${busyboxSource}
              ${nixpkgs.lib.optionalString isArm64 ''
                export HARMONY_NIX_MUSL_SOURCE=${muslSource}
                export HARMONY_NIX_POSTGRES_SOURCE=${postgresSource}
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
          busybox-source = busyboxSource;
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
