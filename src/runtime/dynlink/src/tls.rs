//! Implements ELF TLS Variant II. I highly recommend reading the Fuchsia docs on thread-local
//! storage as prep for this code.

use std::{
    alloc::Layout,
    mem::{align_of, size_of},
    ptr::NonNull,
};

use tracing::{error, trace};
use twizzler_runtime_api::TlsIndex;

// re-export TLS TCB definition
pub use crate::arch::Tcb;
use crate::{
    arch::{get_tls_variant, MINIMUM_TLS_ALIGNMENT},
    compartment::Compartment,
    DynlinkError, DynlinkErrorKind,
};

/// The TLS variant which determines the layout of the TLS region.
pub enum TlsVariant {
    Variant1,
    Variant2,
}

#[derive(Clone)]
pub(crate) struct TlsInfo {
    // DTV needs to start with a generation count.
    gen: u64,
    // When adding modules to the static TLS region
    alloc_size_mods: usize,
    // Calculate the maximum alignment we'll need.
    max_align: usize,
    // Running offset counter.
    offset: usize,
    pub(crate) tls_mods: Vec<TlsModule>,
}

impl TlsInfo {
    pub(crate) fn new(gen: u64) -> Self {
        Self {
            gen,
            alloc_size_mods: Default::default(),
            max_align: MINIMUM_TLS_ALIGNMENT,
            tls_mods: Default::default(),
            offset: 0,
        }
    }

    pub(crate) fn clone_to_new_gen(&self, new_gen: u64) -> Self {
        Self {
            gen: new_gen,
            alloc_size_mods: self.alloc_size_mods,
            max_align: self.max_align,
            tls_mods: self.tls_mods.clone(),
            offset: self.offset,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TlsModule {
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

        let id = match get_tls_variant() {
            TlsVariant::Variant1 => {
                // the first module is aligned and placed after the thread pointer
                if self.tls_mods.is_empty() {
                    // aarch64 reserves the first two words after the thread pointer
                    self.offset = 16;
                    // make sure the current offset from the TP is aligned
                    if !(self.offset as *const u8).is_aligned_to(tm.template_align) {
                        let ptr = self.offset as *const u8;
                        self.offset += ptr.align_offset(tm.template_align);
                    }
                    tm.offset = Some(self.offset);

                    // account for the size of the module
                    self.offset += tm.template_memsz;
                } else {
                    // make sure the offset is aligned
                    if !(self.offset as *const u8).is_aligned_to(tm.template_align) {
                        let ptr = self.offset as *const u8;
                        self.offset += ptr.align_offset(tm.template_align);
                    }

                    // Set the offset so that the region starts aligned and has enough room.
                    tm.offset = Some(self.offset);

                    // account for the size of the module
                    self.offset += tm.template_memsz;
                }
                TlsModId((self.tls_mods.len() + 1) as u64, tm.offset.unwrap())
            }
            TlsVariant::Variant2 => {
                // The first module is placed so that the region ends at the thread pointer. Other
                // regions are just placed at semi-arbitrary positions below that,
                // so we don't need to be as careful about them.
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
                TlsModId((self.tls_mods.len() + 1) as u64, self.offset)
            }
        };

        // Save the module ID + 1 (leave one slot in the DTV for the generation count).
        tm.id = Some(id);
        self.tls_mods.push(tm);
        id
    }

    pub(crate) fn allocate<T>(
        &self,
        _comp: &Compartment,
        alloc_base: NonNull<u8>,
        tcb: T,
    ) -> Result<TlsRegion, DynlinkError> {
        // Given an allocation region, lets find all the interesting pointers.
        let layout = self
            .allocation_layout::<T>()
            .map_err(|err| DynlinkErrorKind::LayoutError { err })?;

        // thread pointer from base allocation
        let thread_pointer = match get_tls_variant() {
            TlsVariant::Variant1 => {
                let mut base = usize::from(alloc_base.addr());
                // this should already be an aligned allocation ...
                base += base & (layout.align() - 1);
                NonNull::new((base + size_of::<Tcb<T>>() - 16) as *mut u8).unwrap()
            }
            TlsVariant::Variant2 => {
                let mut base = usize::from(alloc_base.addr()) + layout.size() - size_of::<Tcb<T>>();
                // Align for the thread pointer and the 1st TLS region.
                base -= base & (layout.align() - 1);
                NonNull::new(base as *mut u8).unwrap()
            }
        };

        let module_start = match get_tls_variant() {
            TlsVariant::Variant1 => {
                // where in the tls region we are after the TCB
                let temp = unsafe { thread_pointer.as_ptr().add(16) };
                // set it to align to the alignment of the first module
                let padding_after_tcb = temp.align_offset(self.tls_mods[0].template_align);
                NonNull::new(unsafe { temp.add(padding_after_tcb) }).unwrap()
            }
            TlsVariant::Variant2 => thread_pointer,
        };

        // calculate the start of the DTV
        let dtv_ptr = match get_tls_variant() {
            TlsVariant::Variant1 => {
                // Variant 1 has the thread pointer pointing to the DTV pointer.
                // offset at this point should be after the static TLS modules
                let after_modules = unsafe { module_start.as_ptr().add(self.offset) };
                let align_padding = after_modules.align_offset(align_of::<usize>());
                let dtv_addr = unsafe { after_modules.add(align_padding).cast::<usize>() };
                NonNull::new(dtv_addr).unwrap()
            }
            TlsVariant::Variant2 => alloc_base.cast(),
        };

        // AA: debug
        let x = module_start.addr().get() - thread_pointer.addr().get();
        trace!("offset from tp start to module start: {}, {:#08x}", x, x);

        let tls_region = TlsRegion {
            gen: self.gen,
            module_top: module_start,
            thread_pointer,
            dtv: dtv_ptr,
            num_dtv_entries: self.dtv_len(),
            alloc_base,
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
        trace!("setting dtv[0] to gen_count {}", self.gen);
        unsafe { *tls_region.dtv.as_ptr() = self.gen as usize };

        // Finally fill out the control block.
        unsafe { (tls_region.get_thread_control_block()).write(Tcb::new(&tls_region, tcb)) };

        Ok(tls_region)
    }

    fn dtv_len(&self) -> usize {
        self.tls_mods.len() + 1
    }

    pub(crate) fn allocation_layout<T>(&self) -> Result<Layout, std::alloc::LayoutError> {
        // Ensure that the alignment is enough for the control block.
        let align = std::cmp::max(self.max_align, align_of::<Tcb<T>>()).next_power_of_two();
        // Region needs space for each module, and we just assume they all need the max alignment.
        // Add two to the mods length for calculating align padding, one for the dtv, one for the
        // tcb.
        let region_size = self.alloc_size_mods + align * (self.tls_mods.len() + 2);
        let dtv_size = self.dtv_len() * size_of::<usize>();
        // We also need space for the control block and the dtv.
        let size = region_size + size_of::<Tcb<T>>() + dtv_size;
        Layout::from_size_align(size, align)
    }
}

impl<T> Tcb<T> {
    pub(crate) fn new(tls_region: &TlsRegion, tcb_data: T) -> Self {
        let self_ptr = unsafe { tls_region.get_thread_control_block() };
        Self {
            self_ptr,
            dtv: tls_region.dtv.as_ptr(),
            dtv_len: tls_region.num_dtv_entries,
            runtime_data: tcb_data,
        }
    }

    pub fn get_addr(&self, index: &TlsIndex) -> Option<*const u8> {
        unsafe {
            let slice = core::slice::from_raw_parts(self.dtv, self.dtv_len);
            Some((slice.get(index.mod_id)? + index.offset) as *const _)
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TlsModId(u64, usize);

impl TlsModId {
    pub fn tls_id(&self) -> u64 {
        self.0
    }

    pub fn offset(&self) -> usize {
        self.1
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct TlsRegion {
    pub gen: u64,
    pub layout: Layout,
    pub alloc_base: NonNull<u8>,
    pub thread_pointer: NonNull<u8>,
    pub dtv: NonNull<usize>,
    pub num_dtv_entries: usize,
    pub module_top: NonNull<u8>,
}

impl TlsRegion {
    pub fn alloc_base(&self) -> *mut u8 {
        self.alloc_base.as_ptr()
    }

    pub fn alloc_layout(&self) -> Layout {
        self.layout
    }

    pub fn get_thread_pointer_value(&self) -> usize {
        self.thread_pointer.as_ptr() as usize
    }

    pub(crate) fn set_dtv_entry(&self, tm: &TlsModule) {
        let dtv_slice =
            unsafe { core::slice::from_raw_parts_mut(self.dtv.as_ptr(), self.num_dtv_entries) };
        let dtv_idx = tm.id.as_ref().unwrap().tls_id() as usize;
        let dtv_val = match get_tls_variant() {
            // TODO: debug, shouldn't this be from thread_pointer top?
            TlsVariant::Variant1 => unsafe { self.module_top.as_ptr().add(tm.offset.unwrap()) },
            TlsVariant::Variant2 => unsafe { self.module_top.as_ptr().sub(tm.offset.unwrap()) },
        };
        trace!("setting dtv entry {} <= {:p}", dtv_idx, dtv_val);
        dtv_slice[dtv_idx] = dtv_val as usize;
    }

    pub(crate) fn copy_in_module(&self, tm: &TlsModule) -> usize {
        unsafe {
            let start = match get_tls_variant() {
                TlsVariant::Variant1 => self.thread_pointer.as_ptr().add(tm.offset.unwrap()),
                TlsVariant::Variant2 => self.module_top.as_ptr().sub(tm.offset.unwrap()),
            };
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

pub use crate::arch::get_current_thread_control_block;
