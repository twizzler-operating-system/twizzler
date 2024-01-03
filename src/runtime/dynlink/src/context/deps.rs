use tracing::debug;

use crate::{
    library::{Library, UnloadedLibrary},
    DynlinkError,
};

use super::{engine::ContextEngine, Context};

impl<Engine: ContextEngine> Context<Engine> {
    /// Get a list of dependencies for this library.
    pub(crate) fn enumerate_needed(
        &self,
        lib: &Library<Engine::Backing>,
    ) -> Result<Vec<UnloadedLibrary>, DynlinkError> {
        debug!("{}: enumerating dependencies", lib);
        let elf = lib.get_elf()?;
        let common = lib.find_common_data()?;

        common
            .dynamic
            .ok_or(DynlinkError::Unknown)?
            .iter()
            .filter_map(|d| match d.d_tag {
                DT_NEEDED => Some({
                    // DT_NEEDED indicates a dependency. Lookup the name in the string table.
                    common
                        .dynsyms_strs
                        .ok_or(DynlinkError::Unknown)
                        .and_then(|strs| {
                            strs.get(d.d_ptr() as usize)
                                .map_err(|_| DynlinkError::Unknown)
                        })
                        .and_then(|name| UnloadedLibrary { name })
                }),
                _ => None,
            })
            .ecollect()
    }
}
