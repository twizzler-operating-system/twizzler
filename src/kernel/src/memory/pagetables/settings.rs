use crate::memory::{context::MappingPerms, map::CacheType};

bitflags::bitflags! {
    /// A collection of flags commonly used for mapping.
    pub struct MappingFlags : u64 {
        const GLOBAL = 1;
    }
}

#[derive(Debug, PartialEq, PartialOrd)]
/// A collection of all the settings for a given mapping.
pub struct MappingSettings {
    // TODO: user perms?
    perms: MappingPerms,
    cache: CacheType,
    flags: MappingFlags,
}

impl MappingSettings {
    /// Constructor for [MappingSettings].
    pub fn new(perms: MappingPerms, cache: CacheType, flags: MappingFlags) -> Self {
        Self {
            perms,
            cache,
            flags,
        }
    }

    /// Get the setting's permissions.
    pub fn perms(&self) -> MappingPerms {
        self.perms
    }

    /// Get the setting's cache info.
    pub fn cache(&self) -> CacheType {
        self.cache
    }

    /// Get the setting's flags.
    pub fn flags(&self) -> MappingFlags {
        self.flags
    }
}
