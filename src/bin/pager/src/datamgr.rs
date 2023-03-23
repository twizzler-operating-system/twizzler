use std::sync::Mutex;

use tickv::{success_codes::SuccessCode, ErrorCode};
use twizzler_abi::pager::ObjectInfo;
use twizzler_object::ObjID;

use crate::store::{Key, KeyKind, KeyValueStore, Storage, BLOCK_SIZE};

#[ouroboros::self_referencing]
struct DataMgrInner {
    read_buffer: [u8; BLOCK_SIZE],
    #[not_covariant]
    #[borrows(mut read_buffer)]
    kv: KeyValueStore<'this>,
}

impl DataMgrInner {
    fn from_storage(store: Storage, size: usize) -> Result<Self, ErrorCode> {
        Self::try_new([0; BLOCK_SIZE], |rb| KeyValueStore::new(store, rb, size))
    }
}

pub struct DataMgr {
    inner: Mutex<DataMgrInner>,
}

impl DataMgr {
    pub fn new(store: Storage, size: usize) -> Result<Self, ErrorCode> {
        Ok(Self {
            inner: Mutex::new(DataMgrInner::from_storage(store, size)?),
        })
    }

    pub async fn lookup_page_entry(&self, id: ObjID, page: u32) -> Result<PageEntry, ErrorCode> {
        let mut inner = self.inner.lock().unwrap();
        let key = Key::new(id, page, KeyKind::PageInfo);
        inner.with_kv_mut(|kv| kv.get(key))
    }

    pub async fn lookup_object_info(&self, id: ObjID) -> Result<ObjectInfo, ErrorCode> {
        let mut inner = self.inner.lock().unwrap();
        let key = Key::new(id, 0, KeyKind::ObjectInfo);
        inner.with_kv_mut(|kv| kv.get(key))
    }

    pub async fn write_object_info(
        &self,
        id: ObjID,
        data: ObjectInfo,
    ) -> Result<SuccessCode, ErrorCode> {
        let mut inner = self.inner.lock().unwrap();
        let key = Key::new(id, 0, KeyKind::ObjectInfo);
        inner.with_kv_mut(|kv| kv.update(key, data))
    }

    pub async fn write_page_entry(
        &self,
        id: ObjID,
        page: u32,
        data: PageEntry,
    ) -> Result<SuccessCode, ErrorCode> {
        let mut inner = self.inner.lock().unwrap();
        let key = Key::new(id, page, KeyKind::PageInfo);
        inner.with_kv_mut(|kv| kv.update(key, data))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct PageEntry {
    start_lba: u64,
}
