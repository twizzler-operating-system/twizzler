use elf::abi::DT_NEEDED;
use tracing::trace;

use super::Context;
use crate::{
    library::{Library, UnloadedLibrary},
    DynlinkError, DynlinkErrorKind,
};

impl Context {
    /// Get a list of dependencies for this library.
    pub(crate) fn enumerate_needed(
        &self,
        lib: &Library,
    ) -> Result<Vec<UnloadedLibrary>, DynlinkError> {
        trace!("{}: enumerating dependencies", lib);
        let elf = lib.get_elf()?;
        let common = elf.find_common_data()?;

        // Iterate over the dynamic table, looking for DT_NEEDED.
        let res = common
            .dynamic
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynamic".to_string(),
            })?
            .iter()
            .filter_map(|d| match d.d_tag {
                DT_NEEDED => Some({
                    // DT_NEEDED indicates a dependency. Lookup the name in the string table.
                    common
                        .dynsyms_strs
                        .ok_or_else(|| DynlinkErrorKind::MissingSection {
                            name: "dynsyms_strs".to_string(),
                        })
                        .and_then(|strs| {
                            strs.get(d.d_ptr() as usize).map_err(|_| {
                                DynlinkErrorKind::MissingSection {
                                    name: "dynsyms_strs".to_string(),
                                }
                            })
                        })
                        .map(|name| UnloadedLibrary {
                            name: name.to_string(),
                        })
                }),
                _ => None,
            })
            .collect::<Vec<_>>();

        DynlinkError::collect(
            DynlinkErrorKind::DepEnumerationFail {
                library: lib.name.clone(),
            },
            res.into_iter().map(|x| x.map_err(|e| e.into())),
        )
    }
}
