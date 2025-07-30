use std::sync::Arc;

use object_store::PagedObjectStore;
use twizzler::{object::ObjID, Result};
use twizzler_abi::{
    pager::{ObjectRange, PhysRange},
    syscall::{BackingType, LifetimeType},
};

use crate::helpers::PAGE;

pub mod virtio;

pub struct MemObjectInfo {
    lt: LifetimeType,
    bt: BackingType,
    len: u64,
}

pub trait MemStore {
    fn set_config_id(&self, id: ObjID) -> Result<()>;
    fn get_config_id(&self) -> Result<ObjID>;

    fn get_map(&self, id: ObjID, range: ObjectRange, phys: &mut Vec<(u64, u64)>) -> Result<()>;

    fn get_object_info(&self, id: ObjID) -> Result<MemObjectInfo>;

    fn flush(&self, id: Option<ObjID>) -> Result<()> {
        Ok(())
    }
}

pub trait MemDevice {
    fn page_size() -> u64
    where
        Self: Sized,
    {
        0x1000
    }

    fn get_physical(&self, start: u64, len: u64) -> Result<PhysRange>;

    fn flush(&self, start: u64, len: u64) -> Result<()> {
        Ok(())
    }
}

pub struct PagedMemStore {
    inner: Arc<dyn MemStore>,
}

impl PagedObjectStore for PagedMemStore {
    fn create_object(&self, id: object_store::ObjID) -> Result<()> {
        todo!()
    }

    fn delete_object(&self, id: object_store::ObjID) -> Result<()> {
        todo!()
    }

    fn len(&self, id: object_store::ObjID) -> Result<u64> {
        self.inner
            .get_object_info(id.into())
            .map(|i| i.len)
            .map_err(|e| e.into())
    }

    fn read_object(&self, id: object_store::ObjID, offset: u64, buf: &mut [u8]) -> Result<usize> {
        todo!()
    }

    fn write_object(&self, id: object_store::ObjID, offset: u64, buf: &[u8]) -> Result<()> {
        todo!()
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn page_in_object<'a>(
        &self,
        id: object_store::ObjID,
        reqs: &'a mut [object_store::PageRequest],
    ) -> Result<usize> {
        todo!();
        Ok(reqs.len())
    }

    fn page_out_object<'a>(
        &self,
        id: object_store::ObjID,
        reqs: &'a mut [object_store::PageRequest],
    ) -> Result<usize> {
        todo!();
        Ok(reqs.len())
    }

    fn enumerate_external(
        &self,
        _id: object_store::ObjID,
    ) -> Result<Vec<object_store::ExternalFile>> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn find_external(&self, _id: object_store::ObjID) -> Result<usize> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
}
