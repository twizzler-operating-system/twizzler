# runtime subdirectory

This subdirectory contains all the crates for the core runtime:

  - .: reference runtime wrapper crate
  - minimal: the minimal (no_std, static linking available) runtime
  - monitor: the monitor implementation
  - monitor-api: the API crate for interaction with the monitor from runtime or user programs
  - reference: the reference runtime implementation. Users should link against the wrapper crate (rt), not this one.
