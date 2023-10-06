use std::{alloc::Layout, ptr::NonNull};

use tracing::{error, trace};

use crate::{arch::MINIMUM_TLS_ALIGNMENT, compartment::CompartmentAlloc, DynlinkError};

pub(crate) struct TlsInfo {
    gen_count: u64,
    alloc_size_mods: usize,
    max_align: usize,
    offset: usize,
    pub(crate) tls_mods: Vec<TlsModule>,
}

impl Default for TlsInfo {
    fn default() -> Self {
        Self {
            gen_count: Default::default(),
            alloc_size_mods: Default::default(),
            max_align: MINIMUM_TLS_ALIGNMENT,
            tls_mods: Default::default(),
            offset: 0,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TlsModule {
    pub is_static: bool,
    pub template_addr: usize,
    pub template_filesz: usize,
    pub template_memsz: usize,
    pub template_align: usize,
    pub offset: Option<usize>,
    pub id: Option<TlsModId>,
}

impl TlsModule {
    pub(crate) fn new_static(
        template_addr: usize,
        template_filesz: usize,
        template_memsz: usize,
        template_align: usize,
    ) -> Self {
        Self {
            is_static: true,
            template_addr,
            template_filesz,
            template_memsz,
            template_align,
            offset: None,
            id: None,
        }
    }
}

impl TlsInfo {
    pub fn insert(&mut self, mut tm: TlsModule) -> TlsModId {
        self.alloc_size_mods += tm.template_memsz;
        self.max_align = std::cmp::max(self.max_align, tm.template_align);
        self.max_align = self.max_align.next_power_of_two();

        if self.tls_mods.len() == 0 {
            self.offset = tm.template_memsz
                + ((tm.template_addr + tm.template_memsz).overflowing_neg().0
                    & (tm.template_align - 1));
        } else {
            self.offset += tm.template_memsz + tm.template_align - 1;
            self.offset -= (self.offset + tm.template_addr) & (tm.template_align - 1);
        }
        tm.offset = Some(self.offset);

        let id = self.tls_mods.len();
        self.tls_mods.push(tm);
        self.gen_count += 1;
        TlsModId((id + 1) as u64)
    }

    pub(crate) fn allocate(&self, alloc_base: NonNull<u8>) -> Result<TlsRegion, DynlinkError> {
        let layout = self
            .allocation_layout()
            .map_err(|_| DynlinkError::Unknown)?;
        let mut base = usize::from(alloc_base.addr()) + layout.size();
        base -= base & (layout.align() - 1);

        let thread_pointer = NonNull::new(base as *mut u8).unwrap();
        let tls_region = TlsRegion {
            module_top: thread_pointer.clone(),
            thread_pointer,
            dtv: alloc_base.cast(),
            num_dtv_entries: self.tls_mods.len() + 1,
            alloc_base,
        };

        for tm in &self.tls_mods {
            if !tm.is_static {
                error!("non-static TLS modules are not supported");
                continue;
            }
            tls_region.copy_in_module(tm);
            tls_region.set_dtv_entry(tm);
        }

        Ok(tls_region)
    }

    pub fn allocation_layout(&self) -> Result<Layout, std::alloc::LayoutError> {
        let region_size = self.alloc_size_mods + self.max_align * self.tls_mods.len();
        let align = self.max_align;

        let size = region_size;

        todo!();
        Layout::from_size_align(size, align)
    }
}

#[derive(Debug)]
#[repr(transparent)]
pub(crate) struct TlsModId(u64);

impl TlsModId {
    pub(crate) fn as_index(&self) -> usize {
        assert!(self.0 >= 2);
        (self.0 - 1) as usize
    }

    pub(crate) fn as_tls_id(&self) -> u64 {
        self.0
    }
}

pub(crate) struct TlsRegion {
    pub alloc_base: NonNull<u8>,
    pub thread_pointer: NonNull<u8>,
    pub dtv: NonNull<usize>,
    pub num_dtv_entries: usize,
    pub module_top: NonNull<u8>,
}

impl Drop for TlsRegion {
    fn drop(&mut self) {
        error!("TODO: drop");
    }
}

impl TlsRegion {
    pub(crate) fn set_dtv_entry(&self, tm: &TlsModule) {
        let dtv_slice =
            unsafe { core::slice::from_raw_parts_mut(self.dtv.as_ptr(), self.num_dtv_entries) };
        dtv_slice[tm.id.as_ref().unwrap().as_tls_id() as usize] = tm.offset.unwrap();
    }

    pub(crate) fn copy_in_module(&self, tm: &TlsModule) -> usize {
        trace!("copy in static region ({:?}", tm);
        unsafe {
            let start = self.module_top.as_ptr().sub(tm.offset.unwrap());
            let src = tm.template_addr as *const u8;
            start.copy_from_nonoverlapping(src, tm.template_filesz);
            start as usize
        }
    }
}
