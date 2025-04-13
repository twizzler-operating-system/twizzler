# Makefile for Rust Project

.PHONY: all bootstrap build run clean

all: bootstrap build run

init:
	git submodule update --init --recursive
	$(MAKE) bootstrap
	$(MAKE) build

bootstrap:
	@echo "Bootstrapping project ..."
	cargo boostrap

build:
	@echo "Building all components ..."
	cargo build-all

run:
	@echo "Starting QEMU ..."
	cargo start-qemu -p=release -q='-nographic'

help:
	@echo "Available targets:"
	@echo "  all        - Bootstrap, build, and run the project (default)"
	@echo "  bootstrap  - Run cargo boostrap"
	@echo "  build      - Run cargo build-all"
	@echo "  run        - Run cargo start-qemu in release mode"
	@echo "  help       - Show this help information"
