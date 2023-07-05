use core::alloc::Layout;

use xmas_elf::program::{self};

use crate::memory::VirtAddr;
use crate::once::Once;
static KERNEL_IMAGE: Once<&'static [u8]> = Once::new();

pub fn init(kernel_image: &'static [u8]) {
    KERNEL_IMAGE.call_once(|| kernel_image);
}

#[derive(Copy, Clone)]
pub struct TlsInfo {
    pub start_addr: VirtAddr,
    pub file_size: usize,
    pub mem_size: usize,
    pub align: usize,
}
pub fn get_tls() -> TlsInfo {
    let elf = xmas_elf::ElfFile::new(*KERNEL_IMAGE.wait()).expect("failed to parse kernel image");
    for ph in elf.program_iter() {
        if let Ok(program::Type::Tls) = ph.get_type() {
            return TlsInfo {
                start_addr: VirtAddr::new(ph.virtual_addr()).unwrap(),
                file_size: ph.file_size() as usize,
                mem_size: ph.mem_size() as usize,
                align: ph.align() as usize,
            };
        }
    }
    panic!("failed to find TLS program header in kernel image");
}

#[derive(Copy, Clone)]
pub enum TlsVariant {
    Variant1,
    Variant2,
}

pub fn init_tls(variant: TlsVariant, tls_template: TlsInfo) -> VirtAddr {
    match variant {
        TlsVariant::Variant1 => variant1(tls_template),
        TlsVariant::Variant2 => variant2(tls_template),
    }
}

fn variant1(tls_template: TlsInfo) -> VirtAddr {
    // TODO: reserved region may be arch specific. aarch64 reserves two
    // words after the thread pointer (TP), before any TLS blocks
    let reserved_bytes = core::mem::size_of::<*const u64>() * 2;
    // the size of the TLS region in memory
    let tls_size = tls_template.mem_size + reserved_bytes;

    // generate a layout where the size is rounded up if not aligned
    let layout =
        Layout::from_size_align(tls_size, tls_template.align).expect("failed to unwrap TLS layout");

    // allocate/initialize a region of memory for the thread-local data
    let tls_region = unsafe {
        // allocate a region of memory initialized to zero
        let tcb_base = alloc::alloc::alloc_zeroed(layout);
        
        // copy from the kernel's ELF TLS to the allocated region of memory
        // the layout of this region in memory is architechture dependent.
        //
        // Architechtures that use TLS Variant I (e.g. ARM) have the thread pointer 
        // point to the start of the TCB and thread-local vars are defined 
        // before this in higher memory addresses. So accessing a thread
        // local var adds some offset to the thread pointer

        // we need a pointer offset of reserved_bytes. add here increments
        // the pointer offset by sizeof u8 bytes.
        let tls_base = tcb_base.add(reserved_bytes);

        core::ptr::copy_nonoverlapping(tls_template.start_addr.as_ptr(), tls_base, tls_template.file_size);

        tcb_base
    };
    
    // the TP points to the base of the TCB which exists in lower memory.
    let tcb_base = VirtAddr::from_ptr(tls_region);
    
    tcb_base
}

const MIN_TLS_ALIGN: usize = 16;

fn variant2(tls_template: TlsInfo) -> VirtAddr {
    let mut tls_size = tls_template.mem_size;
    let alignment = tls_template.align;

    let start_address_ptr = tls_template.start_addr.as_ptr();

    // The rhs of the below expression essentially calculates the amount of padding
    // we will have to introduce within the TLS region in order to achieve the desired
    // alignment.
    tls_size += (((!tls_size) + 1) - (start_address_ptr as usize)) & (alignment - 1);

    let tls_align = core::cmp::max(alignment, MIN_TLS_ALIGN);
    let full_tls_size = (core::mem::size_of::<*const u8>() + tls_size + tls_align + MIN_TLS_ALIGN
        - 1)
        & ((!MIN_TLS_ALIGN) + 1);

    let layout =
        Layout::from_size_align(full_tls_size, tls_align).expect("failed to unwrap TLS layout");

    let tls = unsafe {
        let tls = alloc::alloc::alloc_zeroed(layout);

        core::ptr::copy_nonoverlapping(start_address_ptr, tls, tls_template.file_size);

        tls
    };
    let tcb_base = VirtAddr::from_ptr(tls).offset(full_tls_size).unwrap();

    unsafe { *(tcb_base.as_mut_ptr()) = tcb_base.raw() };

    tcb_base
}