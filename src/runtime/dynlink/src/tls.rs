//! Implements ELF TLS Variant II. I highly recommend reading the Fuchsia docs on thread-local storage as prep for this code.

use std::{alloc::Layout, mem::align_of, mem::size_of, ptr::NonNull};

use tracing::{error, trace};

use crate::{arch::MINIMUM_TLS_ALIGNMENT, compartment::CompartmentRef, DynlinkError};

pub(crate) struct TlsInfo {
    // DTV needs to start with a generation count.
    gen_count: u64,
    // When adding modules to the static TLS region
    alloc_size_mods: usize,
    // Calculate the maximum alignment we'll need.
    max_align: usize,
    // Running offset counter.
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
        // Track size and alignment requirement changes.
        self.alloc_size_mods += tm.template_memsz;
        self.max_align = std::cmp::max(self.max_align, tm.template_align);
        self.max_align = self.max_align.next_power_of_two();

        // The first module is placed so that the region ends at the thread pointer. Other regions
        // are just placed at semi-arbitrary positions below that, so we don't need to be as careful
        // about them.
        if self.tls_mods.is_empty() {
            // Set the offset so that the template ends at the thread pointer.
            self.offset = tm.template_memsz
                + ((tm.template_addr + tm.template_memsz).overflowing_neg().0
                    & (tm.template_align - 1));
        } else {
            // Set the offset so that the region starts aligned and has enough room.
            self.offset += tm.template_memsz + tm.template_align - 1;
            self.offset -= (self.offset + tm.template_addr) & (tm.template_align - 1);
        }
        tm.offset = Some(self.offset);

        // Save the module ID + 1 (leave one slot in the DTV for the generation count).
        let id = TlsModId((self.tls_mods.len() + 1) as u64, self.offset);
        tm.id = Some(id);
        self.tls_mods.push(tm);
        self.gen_count += 1;
        id
    }

    pub(crate) fn allocate<T>(
        &self,
        comp: &CompartmentRef,
        alloc_base: NonNull<u8>,
        tcb: T,
    ) -> Result<TlsRegion, DynlinkError> {
        let layout = self
            .allocation_layout::<T>()
            .map_err(|_| DynlinkError::Unknown)?;

        // Given an allocation regions, lets find all the interesting pointers.
        let mut base = usize::from(alloc_base.addr()) + layout.size();
        // Align for the thread pointer and the 1st TLS region.
        base -= base & (layout.align() - 1);
        let thread_pointer = NonNull::new(base as *mut u8).unwrap();
        let tls_region = TlsRegion {
            module_top: thread_pointer,
            thread_pointer,
            dtv: alloc_base.cast(),
            num_dtv_entries: self.dtv_len(),
            alloc_base,
            comp: comp.clone(),
            layout,
        };

        // Each TLS module gets part of the region, data copied from the template.
        for tm in &self.tls_mods {
            if !tm.is_static {
                error!("non-static TLS modules are not supported");
                continue;
            }
            tls_region.copy_in_module(tm);
            tls_region.set_dtv_entry(tm);
        }

        // Write the gen count.
        trace!("setting dtv[0] to gen_count {}", self.gen_count);
        unsafe { *tls_region.dtv.as_ptr() = self.gen_count as usize };

        // Finally fill out the control block.
        unsafe {
            (tls_region.thread_pointer.as_ptr() as *mut Tcb<T>).write(Tcb::new(&tls_region, tcb))
        };

        Ok(tls_region)
    }

    fn dtv_len(&self) -> usize {
        self.tls_mods.len() + 1
    }

    pub(crate) fn allocation_layout<T>(&self) -> Result<Layout, std::alloc::LayoutError> {
        // Region needs space for each module, and we just assume they all need the max alignment.
        let region_size = self.alloc_size_mods + self.max_align * self.tls_mods.len();
        // Ensure that the alignment is enough for the control block.
        let align = std::cmp::max(self.max_align, align_of::<Tcb<T>>()).next_power_of_two();
        let dtv_size = self.dtv_len() * size_of::<usize>();
        // We also need space for the control block and the dtv.
        let size = region_size + size_of::<Tcb<T>>() + dtv_size;
        Layout::from_size_align(size, align)
    }
}

#[repr(C)]
pub(crate) struct Tcb<T> {
    self_ptr: *const Tcb<T>,
    dtv: *const usize,
    dtv_len: usize,
    runtime_data: T,
}

impl<T> Tcb<T> {
    pub(crate) fn new(tls_region: &TlsRegion, tcb_data: T) -> Self {
        let self_ptr = tls_region.thread_pointer.as_ptr() as *mut Tcb<T>;
        Self {
            self_ptr,
            dtv: tls_region.dtv.as_ptr(),
            dtv_len: tls_region.num_dtv_entries,
            runtime_data: tcb_data,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TlsModId(u64, usize);

impl TlsModId {
    pub(crate) fn tls_id(&self) -> u64 {
        self.0
    }

    pub(crate) fn offset(&self) -> usize {
        self.1
    }
}

#[derive(Debug)]
pub struct TlsRegion {
    pub(crate) comp: CompartmentRef,
    pub(crate) layout: Layout,
    pub(crate) alloc_base: NonNull<u8>,
    pub(crate) thread_pointer: NonNull<u8>,
    pub(crate) dtv: NonNull<usize>,
    pub(crate) num_dtv_entries: usize,
    pub(crate) module_top: NonNull<u8>,
}

impl Drop for TlsRegion {
    fn drop(&mut self) {
        let _ = self.comp.with_inner_mut(|inner| {
            unsafe { inner.dealloc(self.alloc_base, self.layout) };
        });
    }
}

impl TlsRegion {
    pub fn get_thread_pointer_value(&self) -> usize {
        self.thread_pointer.as_ptr() as usize
    }

    pub(crate) fn set_dtv_entry(&self, tm: &TlsModule) {
        let dtv_slice =
            unsafe { core::slice::from_raw_parts_mut(self.dtv.as_ptr(), self.num_dtv_entries) };
        let dtv_idx = tm.id.as_ref().unwrap().tls_id() as usize;
        let dtv_val = unsafe { self.module_top.as_ptr().sub(tm.offset.unwrap()) };
        trace!("setting dtv entry {} <= {:p}", dtv_idx, dtv_val);
        dtv_slice[dtv_idx] = dtv_val as usize;
    }

    pub(crate) fn copy_in_module(&self, tm: &TlsModule) -> usize {
        unsafe {
            let start = self.module_top.as_ptr().sub(tm.offset.unwrap());
            let src = tm.template_addr as *const u8;
            trace!(
                "copy in static region {:p} => {:p} (filesz={}, memsz={})",
                src,
                start,
                tm.template_filesz,
                tm.template_memsz
            );
            start.copy_from_nonoverlapping(src, tm.template_filesz);
            start as usize
        }
    }
}
