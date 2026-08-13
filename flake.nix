{
  description = "Twizzler Development Environment";
  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    { self, ... }@inputs:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                inputs.self.overlays.default
              ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        rustToolchain = inputs.fenix.packages.${prev.stdenv.hostPlatform.system}.fromToolchainFile {
          file = ./rust-toolchain;
          sha256 = "sha256-tX7DPHB0+utlIgFILpRy1McUA3L4qizFg9cFCfmZ39M=";
        };
      };
      devShells = forEachSupportedSystem (
        { pkgs }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              openssl
              curl
              pkg-config
              cargo-deny
              cargo-edit
              cargo-watch
              ninja
              cmake
              qemu
              qemu_kvm
              # this doesnt work on macos
              # bridge-utils
              virt-manager
              libvirt
              libclang
              mdbook

              e2fsprogs
              llvmPackages.lld
            ];
            env = {
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
              LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";

              # to compile blake3 with asm features without erroring on nixos
              CFLAGS = "-Wa,--compress-debug-sections=none";
              ASFLAGS = "--compress-debug-sections=none";
              CFLAGS_x86_64_unknown_none = "-Wa,--compress-debug-sections=none";
              ASFLAGS_x86_64_unknown_none = "--compress-debug-sections=none";
              TARGET_CFLAGS = "-Wa,--compress-debug-sections=none";
            };
            # NIX_LDFLAGS carries -L search paths for build-time linking, but
            # tools compiled by the build (e.g. xtask, cargo's own build
            # scripts) also need these on LD_LIBRARY_PATH to dynamically
            # load libs like libz.so.1 / libcurl.so.4 at run time.
            shellHook = ''
              export LD_LIBRARY_PATH="$(printf '%s' "$NIX_LDFLAGS" | tr ' ' '\n' | sed -n 's/^-L//p' | paste -sd: -):$LD_LIBRARY_PATH"
            '';
          };
        }
      );
    };
}
