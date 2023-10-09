use std::mem::size_of;

use elf::abi::{DT_INIT, DT_INIT_ARRAY, DT_INIT_ARRAYSZ, DT_PREINIT_ARRAY, DT_PREINIT_ARRAYSZ};
use tracing::{debug, warn};

use crate::DynlinkError;

use super::Library;

impl Library {
    pub(crate) fn get_ctor_info(&self) -> Result<CtorInfo, DynlinkError> {
        let dynamic = self.get_elf()?.dynamic()?.ok_or(DynlinkError::Unknown)?;
        // If this isn't present, just call it 0, since if there's an init_array, this entry must be present in valid ELF files.
        let init_array_len = dynamic
            .iter()
            .find_map(|d| {
                if d.d_tag == DT_INIT_ARRAYSZ {
                    Some((d.d_val() as usize) / size_of::<usize>())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        // Init array is a pointer to an array of function pointers.
        let init_array = dynamic.iter().find_map(|d| {
            if d.d_tag == DT_INIT_ARRAY {
                self.laddr::<u8>(d.d_ptr())
            } else {
                None
            }
        });

        // Legacy _init call. Supported for, well, legacy.
        let leg_init = dynamic.iter().find_map(|d| {
            if d.d_tag == DT_INIT {
                self.laddr::<u8>(d.d_ptr())
            } else {
                None
            }
        });

        if dynamic
            .iter()
            .find(|d| d.d_tag == DT_PREINIT_ARRAY)
            .is_some()
        {
            if dynamic
                .iter()
                .find(|d| d.d_tag == DT_PREINIT_ARRAYSZ)
                .is_some_and(|d| d.d_val() > 0)
            {
                warn!("{}: PREINIT_ARRAY is unsupported", self);
            }
        }

        debug!(
            "{}: ctor info: init_array: {:?} len={}, legacy: {:?}",
            self, init_array, init_array_len, leg_init
        );
        Ok(CtorInfo {
            legacy_init: leg_init.map(|p| p as usize).unwrap_or_default(),
            init_array: init_array.map(|p| p as usize).unwrap_or_default(),
            init_array_len,
        })
    }
}

#[allow(dead_code)]
pub(crate) struct CtorInfo {
    pub legacy_init: usize,
    pub init_array: usize,
    pub init_array_len: usize,
}
