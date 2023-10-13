use tracing::{debug, error};

use crate::{DynlinkError, ECollector};

use super::{Library, LibraryLoader};

use elf::abi::DT_NEEDED;

impl Library {
    /// Get a list of dependencies for this library.
    pub(crate) fn enumerate_needed(
        &self,
        loader: &mut impl LibraryLoader,
    ) -> Result<Vec<Library>, DynlinkError> {
        debug!("{}: enumerating dependencies", self);
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;

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
                        .and_then(|name| {
                            // We found the name, ask the loader to load the library for us.
                            let dep = loader.open(name);
                            if dep.is_err() {
                                error!("failed to resolve library {} (needed by {})", name, self);
                            }
                            dep.map(|dep| Library::new(dep, name.to_string()))
                        })
                }),
                _ => None,
            })
            .ecollect()
    }
}
