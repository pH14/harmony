# SPDX-License-Identifier: AGPL-3.0-or-later
{
  description = "Locked, offline Harmony platform and workload image builders";

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
          runcX86 = pkgs.fetchurl {
            url = "https://github.com/opencontainers/runc/releases/download/v1.5.0/runc.amd64";
            sha256 = "0363e69bebd3a027d1239364ab9b4f4873f6bc4e7a7878e94b4ea59f08551297";
          };
          runcSource = pkgs.fetchurl {
            url = "https://github.com/opencontainers/runc/archive/refs/tags/v1.5.0.tar.gz";
            sha256 = "1bcd55af6081cf080557b148d906f64e6e4ca886d8345fe4b5510a90b52815d1";
          };
          goArmBootstrap = pkgs.fetchurl {
            url = "https://dl.google.com/go/go1.25.0.linux-arm64.tar.gz";
            sha256 = "05de75d6994a2783699815ee553bd5a9327d8b79991de36e38b66862782f54ae";
          };
          postgresSource = pkgs.fetchurl {
            url = "https://ftp.postgresql.org/pub/source/v17.10/postgresql-17.10.tar.bz2";
            sha256 = "078a03516dcdbdb705fecaf415ea3d13a956c589e46f09fed68a06fb00598c90";
          };
          commonRuntimeInputs = with pkgs; [
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
          ];
          nativeRuntimeInputs = commonRuntimeInputs
            ++ nixpkgs.lib.optionals isArm64 [
              pkgs.gcc
              pkgs.rsync
            ]
            ++ nixpkgs.lib.optionals (!isArm64) [
              pkgs.elfutils
              pkgs.elfutils.dev
              pkgs.gcc13
              pkgs.go
              pkgs.glibc.static
              pkgs.openssl
              pkgs.openssl.dev
              pkgs.pkg-config
            ];
          platformBuilder = pkgs.writeShellApplication {
            name = "harmony-build-guest-images";
            runtimeInputs = nativeRuntimeInputs;
            text = ''
              export HARMONY_NIX_SOURCE=${self.outPath}
              export HARMONY_NIX_LINUX_SOURCE=${linuxSource}
              export HARMONY_NIX_BUSYBOX_SOURCE=${busyboxSource}
              ${nixpkgs.lib.optionalString isArm64 ''
                export HARMONY_NIX_MUSL_SOURCE=${muslSource}
                export HARMONY_NIX_RUNC_SOURCE=${runcSource}
                export HARMONY_NIX_GO_ARM_BOOTSTRAP=${goArmBootstrap}
              ''}
              ${nixpkgs.lib.optionalString (!isArm64) ''
                export HARMONY_NIX_RUNC_X86_SOURCE=${runcX86}
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
          workloadBuilder = pkgs.writeShellApplication {
            name = "harmony-build-workload-images";
            runtimeInputs = nativeRuntimeInputs;
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
              exec ${./workloads/guest-images/nix/build-guest-images.sh} "$@"
            '';
          };
        in {
          busybox-source = busyboxSource;
          platform-guest-images = platformBuilder;
          workload-guest-images = workloadBuilder;
          guest-images = platformBuilder;
          default = platformBuilder;
        });

      apps = forAllSystems (system: {
        platform-guest-images = {
          type = "app";
          program = "${self.packages.${system}.platform-guest-images}/bin/harmony-build-guest-images";
        };
        workload-guest-images = {
          type = "app";
          program = "${self.packages.${system}.workload-guest-images}/bin/harmony-build-workload-images";
        };
        guest-images = {
          type = "app";
          program = "${self.packages.${system}.platform-guest-images}/bin/harmony-build-guest-images";
        };
        default = self.apps.${system}.platform-guest-images;
      });
    };
}
