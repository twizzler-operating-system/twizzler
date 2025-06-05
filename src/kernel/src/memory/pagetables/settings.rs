use twizzler_abi::{device::CacheType, object::Protections};

bitflags::bitflags! {
    /// A collection of flags commonly used for mapping.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct MappingFlags : u64 {
        /// The mapping is global, and may persist in the TLB across context switches.
        const GLOBAL = 1;
        /// The mapping is accessible by userspace.
        const USER = 2;
    }
}

#[derive(Debug, PartialEq, PartialOrd, Clone, Eq, Copy)]
/// A collection of all the settings for a given mapping.
pub struct MappingSettings {
    perms: Protections,
    cache: CacheType,
    flags: MappingFlags,
}

impl MappingSettings {
    /// Constructor for [MappingSettings].
    pub fn new(perms: Protections, cache: CacheType, flags: MappingFlags) -> Self {
        Self {
            perms,
            cache,
            flags,
        }
    }

    /// Get the setting's permissions.
    pub fn perms(&self) -> Protections {
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

    pub fn default_user() -> Self {
        Self::new(Protections::all(), CacheType::WriteBack, MappingFlags::USER)
    }

    pub fn default_kernel() -> Self {
        Self::new(
            Protections::all(),
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        )
    }

    pub fn default_cachetype(cache: CacheType) -> Self {
        Self::new(Protections::all(), cache, MappingFlags::USER)
    }
}
