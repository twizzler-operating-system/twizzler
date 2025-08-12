#!/usr/bin/env bash

MAC_PACKAGES=(
            "qemu"
            "e2fsprogs"
            "ninja"
) 

DEBIAN_PKGS=(
    qemu-system
    qemu-utils
    qemu-kvm
    bridge-utils
    cpu-checker
    libvirt-daemon-system
    libvirt-clients
    virt-manager
    curl
    build-essential
    pkg-config
    libssl-dev
    python3
    python3-pip
    cmake
    ninja-build
    sudo
    git
    clang
)

FEDORA_PKGS=(
    qemu-system-x86
    qemu-img
    qemu-kvm
    bridge-utils
    libvirt
    libvirt-daemon-kvm
    virt-install
    virt-manager
    curl
    @development-tools
    pkgconf-pkg-config
    openssl-devel
    python3
    python3-pip
    cmake
    ninja-build
    sudo
    git
    clang
)

ARCH_PKGS=(
    qemu
    qemu-arch-extra
    bridge-utils
    libvirt
    virt-manager
    curl
    base-devel
    pkgconf
    openssl
    python
    python-pip
    cmake
    ninja
    sudo
    git
    clang
)


install_linux() {
    echo "Detected Linux"
    source /etc/os-release || { echo "Cannot detect Linux distro."; exit 1; }

    case "$ID" in
        ubuntu|debian)
            PKGS=("${DEBIAN_PKGS[@]}")
            PM_INSTALL="sudo apt-get update && sudo apt-get install -y"
            ;;
        fedora|rhel|centos)
            PKGS=("${FEDORA_PKGS[@]}")
            PM_INSTALL="sudo dnf install -y"
            ;;
        arch|manjaro)
            PKGS=("${ARCH_PKGS[@]}")
            PM_INSTALL="sudo pacman -Sy --noconfirm"
            ;;
        *)
            echo "⚠️ Unsupported Linux distro: $ID"
            echo "Please install the following packages manually: ${DEBIAN_PKGS[*]}"
            exit 1
            ;;
    esac

    eval "$PM_INSTALL ${PKGS[*]}"
}


install_macos() {
    echo "Detected macOS"

    # Xcode Command Line Tools
    if ! xcode-select -p &>/dev/null; then
        echo "Installing Xcode Command Line Tools..."
        xcode-select --install || true
        until xcode-select -p &>/dev/null; do
            sleep 5
        done
        echo "Xcode Command Line Tools installed."
    else
        echo "Xcode Command Line Tools already installed."
    fi

    # Homebrew
    if ! command -v brew >/dev/null 2>&1; then
        echo "Homebrew is required to continue installing packages."
        echo "After installing Homebrew, re-run this script."
        exit 1
    fi

    brew install "${MAC_PACKAGES[@]}"
}

OS_TYPE="$(uname -s)"
case "$OS_TYPE" in
    Linux*)  install_linux ;;
    Darwin*) install_macos ;;
    *)
        echo "Unsupported OS: $OS_TYPE"
        echo "Please open an issue so we can help you."
        exit 1
        ;;
esac
