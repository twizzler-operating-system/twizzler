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
          default = pkgs.mkShellNoCC {
            packages = with pkgs; [
              rustToolchain
              openssl
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
            ];
            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
              # Required by bindgen to find libclang.so
              LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            };
          };
        }
      );
    };
}
