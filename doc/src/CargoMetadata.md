# Cargo metadata for Twizzler crates built inside xtask

The xtask program organizes the build into a series of "collections" that get built in different environments. There are:

 - Tools (targets build system)
 - Kernel (targets arch-machine-none)
 - Userspace (targets arch-machine-twizzler, optional, default yes)
 - Userspace-static (targets arch-machine-twizzler-minruntime, optional, default yes)
 - Userspace-tests (targets arch-machine-twizzler-minruntime, optional, default no)
 - Kernel-tests (targets arch-machine-none, optional, default no)

Programs may select which collection to be compiled in based on the metadata value set in Cargo.toml, described in more detail below.

## Static versus non-static builds
Twizzler currently builds packages in two `target_env` settings: "minruntime" and "". This translates to two triples that are used for userspace twizzler programs: arch-machine-twizzler, and arch-machine-twizzler-minruntime. The minruntime variant is defined to be for statically linked programs, using the default minimal runtime provided by twizzler-abi. Such crates can declare that they should be compiled only in the minruntime collection by setting the key `package.metadata.twizzler-build` to "static" in Cargo.toml:

```{toml}
[package.metadata]
twizzler-build = "static"
```

## Tools

Tools should be placed in the tools subdirectory, and should set the `package.metadata.twizzler-build` key to "tool" in Cargo.toml:

```{toml}
[package.metadata]
twizzler-build = "tool"
```

## The kernel and xtask
Both the kernel and xtask themselves set the `package.metadata.twizzler-build` key to "kernel" or "xtask". Programs should not use these values. 