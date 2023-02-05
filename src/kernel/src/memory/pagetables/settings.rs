use crate::memory::{context::MappingPerms, map::CacheType};

bitflags::bitflags! {
    pub struct MappingFlags : u64 {
        const GLOBAL = 1;
    }
}

#[derive(Debug, PartialEq, PartialOrd)]
pub struct MappingSettings {
    perms: MappingPerms,
    cache: CacheType,
    flags: MappingFlags,
}

impl MappingSettings {
    pub fn new(perms: MappingPerms, cache: CacheType, flags: MappingFlags) -> Self {
        Self {
            perms,
            cache,
            flags,
        }
    }

    pub fn perms(&self) -> MappingPerms {
        self.perms
    }

    pub fn cache(&self) -> CacheType {
        self.cache
    }

    pub fn flags(&self) -> MappingFlags {
        self.flags
    }
}
