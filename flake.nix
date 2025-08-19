{
  description = "Dev environment for QEMU, libvirt, and build tools";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let pkgs = import nixpkgs { inherit system; };
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            # Virtualization / QEMU
            qemu
            qemu_kvm
            bridge-utils
            libvirt
            virt-manager

            # Build / Development
            curl
            build-essential
            pkg-config
            openssl
            python3
            python3Packages.pip
            cmake
            ninja
            git
            clang
            sudo
          ];

          shellHook = ''
            echo "🐧 Development environment ready."
            echo "Linux virtualization tools: QEMU, libvirt, virt-manager"
            echo "Build tools: gcc/clang, cmake, ninja, python, pip"
          '';
        };
      });
}
