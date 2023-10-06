use std::mem::size_of;

use elf::abi::{DT_INIT, DT_INIT_ARRAY, DT_INIT_ARRAYSZ};
use tracing::debug;

use crate::DynlinkError;

use super::Library;

impl Library {
    pub(crate) fn get_ctor_info(&self) -> Result<CtorInfo, DynlinkError> {
        let dynamic = self.get_elf()?.dynamic()?.ok_or(DynlinkError::Unknown)?;
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
        let leg_init = dynamic.iter().find_map(|d| {
            if d.d_tag == DT_INIT {
                self.laddr::<u8>(d.d_ptr())
            } else {
                None
            }
        });
        let init_array = dynamic.iter().find_map(|d| {
            if d.d_tag == DT_INIT_ARRAY {
                self.laddr::<u8>(d.d_ptr())
            } else {
                None
            }
        });
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
