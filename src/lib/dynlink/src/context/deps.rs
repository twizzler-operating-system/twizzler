use elf::abi::DT_NEEDED;
use tracing::trace;

use super::Context;
use crate::{
    library::{Library, UnloadedLibrary},
    DynlinkError, DynlinkErrorKind, Vec, SMALL_VEC_SIZE,
};

impl Context {
    /// Get a list of dependencies for this library.
    pub(crate) fn enumerate_needed(
        &self,
        lib: &Library,
    ) -> Result<Vec<UnloadedLibrary, SMALL_VEC_SIZE>, DynlinkError> {
        trace!("{}: enumerating dependencies", lib);
        let common = lib.get_elf_common()?;

        // Iterate over the dynamic table, looking for DT_NEEDED.
        let res = common
            .dynamic
            .as_ref()
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynamic".into(),
            })?
            .iter()
            .filter_map(|d| match d.d_tag {
                DT_NEEDED => Some({
                    // DT_NEEDED indicates a dependency. Lookup the name in the string table.
                    common
                        .dynsyms_strs
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::MissingSection {
                            name: "dynsyms_strs".into(),
                        })
                        .and_then(|strs| {
                            strs.get(d.d_ptr() as usize).map_err(|_| {
                                DynlinkErrorKind::MissingSection {
                                    name: "dynsyms_strs".into(),
                                }
                            })
                        })
                        .map(|name| UnloadedLibrary::new(name))
                }),
                _ => None,
            })
            .collect::<std::vec::Vec<_>>();

        DynlinkError::collect(
            DynlinkErrorKind::DepEnumerationFail {
                library: lib.name.as_str().into(),
            },
            res.into_iter().map(|x| x.map_err(|e| e.into())),
        )
    }
}
