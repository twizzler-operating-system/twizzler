use secgate::util::Descriptor;
use twizzler_runtime_api::ObjID;

use super::Monitor;
use crate::gates::{LibraryInfo, LoadLibraryError};

impl Monitor {
    pub fn get_library_info(&self, caller: ObjID, desc: Descriptor) -> LibraryInfo {
        todo!()
    }

    pub fn get_library_handle(
        &self,
        caller: ObjID,
        comp: Option<Descriptor>,
        num: usize,
    ) -> Option<Descriptor> {
        todo!()
    }

    pub fn load_library(&self, caller: ObjID, id: ObjID) -> Result<Descriptor, LoadLibraryError> {
        todo!()
    }

    pub fn drop_library_handle(&self, caller: ObjID, desc: Descriptor) {
        todo!()
    }
}
