use std::{error::Error, io::ErrorKind};

use object_store::{
    DevicePage, PageRequest, PagedObjectStore, PagedPhysMem, PhysRange, Vec, INLINE_LEN,
};
use pager_store::{
    Extent, RegisterFn, Status, StoreTable, ABI_VERSION, EXTENT_DATA, EXTENT_PHYS, LIB_PREFIX,
    PAGE_SIZE, REGISTER_SYMBOL,
};
use twizzler::{object::ObjID, Result};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
};

use crate::{helpers::PAGE, physrw::fill_physical_pages, PAGER_CTX};

pub type DynStore = dyn PagedObjectStore + Send + Sync;

/// Serves every object whose ID satisfies `id & mask == value`.
pub struct Route {
    pub mask: u128,
    pub value: u128,
    pub store: Box<DynStore>,
}

impl Route {
    pub fn claims(&self, id: ObjID) -> bool {
        id.raw() & self.mask == self.value
    }
}

/// Most pages asked of a loaded store in one call, which bounds the bounce buffer.
const MAX_BATCH: u32 = 512;

fn kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

/// Load every store library in the initrd.
pub fn load_stores() -> std::vec::Vec<Route> {
    kernel_init_info()
        .names()
        .iter()
        .map(|n| n.name())
        .filter(|n| n.starts_with(LIB_PREFIX) && n.ends_with(".so"))
        .filter_map(|name| {
            load_store(name)
                .inspect(|_| tracing::info!("loaded object store {}", name))
                .inspect_err(|e| tracing::warn!("failed to load object store {}: {}", name, e))
                .ok()
        })
        .collect()
}

fn load_store(name: &str) -> std::result::Result<Route, Box<dyn Error>> {
    let lib = unsafe { libloading::Library::new(name) }?;
    let register = unsafe { lib.get::<RegisterFn>(REGISTER_SYMBOL) }?;
    let table = unsafe { register().as_ref() }.ok_or("null store table")?;
    if table.abi_version != ABI_VERSION {
        return Err(format!(
            "abi version {}, expected {}",
            table.abi_version, ABI_VERSION
        )
        .into());
    }
    // Stores are never unloaded.
    std::mem::forget(lib);
    Ok(Route {
        mask: table.id_mask,
        value: table.id_value,
        store: Box::new(LoadedStore { table }),
    })
}

struct LoadedStore {
    table: &'static StoreTable,
}

fn check(status: Status) -> Result<()> {
    match status {
        Status::OK => Ok(()),
        Status::NOT_FOUND => Err(ErrorKind::NotFound.into()),
        _ => Err(ErrorKind::Other.into()),
    }
}

impl LoadedStore {
    fn page_in_req(&self, id: u128, req: &mut PageRequest) -> Result<()> {
        let nr_pages = req.nr_pages.min(MAX_BATCH);
        let mut data = vec![0u8; nr_pages as usize * PAGE_SIZE];
        let mut out = [Extent::default(); INLINE_LEN];
        let mut count = 0;
        check(unsafe {
            (self.table.page_in)(
                id,
                req.start_page as u64,
                nr_pages,
                data.as_mut_ptr(),
                out.as_mut_ptr(),
                out.len() as u32,
                &mut count,
            )
        })?;

        let ctx = PAGER_CTX.get().unwrap();
        let mut page = 0;
        for ext in &out[..(count as usize).min(out.len())] {
            if page + ext.nr_pages > nr_pages {
                return Err(ErrorKind::InvalidData.into());
            }
            match ext.kind {
                EXTENT_DATA => {
                    let mut done = 0;
                    while done < ext.nr_pages {
                        let obj_page = req.start_page + (page + done) as i64;
                        let (start, n) =
                            match ctx.data.try_alloc_pages(obj_page, ext.nr_pages - done) {
                                Ok(x) => x,
                                Err(mw) => {
                                    mw.wait();
                                    continue;
                                }
                            };
                        let range = PhysRange::new(start, start + n as u64 * PAGE);
                        let off = (page + done) as usize * PAGE_SIZE;
                        let buf = &data[off..(off + n as usize * PAGE_SIZE)];
                        if let Err(e) = fill_physical_pages(buf, range) {
                            ctx.data.add_memory_range(range);
                            return Err(e);
                        }
                        req.phys_list.push(PagedPhysMem::new(range).completed());
                        done += n;
                    }
                }
                EXTENT_PHYS => {
                    let range = PhysRange::new(ext.phys, ext.phys + ext.nr_pages as u64 * PAGE);
                    req.phys_list
                        .push(PagedPhysMem::new(range).completed().wired());
                }
                _ => return Err(ErrorKind::InvalidData.into()),
            }
            page += ext.nr_pages;
        }
        if page == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        req.nr_pages = page;
        req.completed = page;
        Ok(())
    }
}

impl PagedObjectStore for LoadedStore {
    fn create_object(&self, _id: u128) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn delete_object(&self, _id: u128) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn len(&self, id: u128) -> Result<u64> {
        let mut len = 0;
        check(unsafe { (self.table.len)(id, &mut len) })?;
        Ok(len)
    }

    fn read_object(&self, _id: u128, _offset: u64, _buf: &mut [u8]) -> Result<usize> {
        Err(ErrorKind::Unsupported.into())
    }

    fn write_object(&self, _id: u128, _offset: u64, _buf: &[u8]) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn get_disk_blocks(
        &self,
        _id: u128,
        _start_page: u64,
        _nr_pages: u32,
        _create: bool,
        _out: &mut Vec<DevicePage, INLINE_LEN>,
    ) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn page_in_object<'a>(&self, id: u128, reqs: &'a mut [PageRequest]) -> Result<usize> {
        for req in reqs.iter_mut() {
            self.page_in_req(id, req)?;
        }
        Ok(reqs.len())
    }

    fn page_out_object<'a>(
        &self,
        _id: u128,
        _reqs: &'a mut [PageRequest],
        _true_len: Option<u64>,
    ) -> Result<usize> {
        Err(ErrorKind::Unsupported.into())
    }
}
