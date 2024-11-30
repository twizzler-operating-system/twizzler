# runtime subdirectory

This subdirectory contains all the crates for the core runtime:

  - dynlink: the dynamic linker code
  - minruntime: the minimal (no_std, static linking available) runtime
  - monitor: the monitor implementation
  - monitor-api: the API crate for interaction with the monitor from runtime or user programs
  - rt: reference runtime wrapper crate
  - rt-impl: the reference runtime implementation. Users should link against the wrapper crate (rt), not this one.
  - secgate: secure gate types and utility functions
