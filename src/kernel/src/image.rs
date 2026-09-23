use core::alloc::Layout;

use xmas_elf::program::{self};

use crate::{memory::VirtAddr, once::Once};
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
    // The block follows the 16 reserved bytes at the thread pointer, padded so that it is
    // congruent to the template address modulo its alignment: that is where lld's local-exec
    // offsets point, assuming TP itself is aligned.
    let align = tls_template.align.max(1);
    let block_offset =
        16 + ((tls_template.start_addr.raw() as usize).wrapping_sub(16) & (align - 1));
    let layout = Layout::from_size_align(block_offset + tls_template.mem_size, align.max(16))
        .expect("failed to unwrap TLS layout");

    let tcb_base = unsafe {
        let tcb_base = alloc::alloc::alloc_zeroed(layout);
        core::ptr::copy_nonoverlapping(
            tls_template.start_addr.as_ptr(),
            tcb_base.add(block_offset),
            tls_template.file_size,
        );
        tcb_base
    };

    VirtAddr::from_ptr(tcb_base)
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
    let tcb_base = VirtAddr::from_ptr(tls).offset(tls_size).unwrap();

    unsafe { *(tcb_base.as_mut_ptr()) = tcb_base.raw() };

    tcb_base
}

#[cfg(test)]
mod test {
    use twizzler_kernel_macros::kernel_test;

    // the correct value that the TLS var should be set to
    const TLS_TEST_MAGIC: u64 = 0x900dc0ffee123abc;

    #[thread_local]
    static SOME_INT: u64 = TLS_TEST_MAGIC;

    #[kernel_test]
    fn tls_test() {
        // get the initial value of TLS var
        assert_eq!(
            SOME_INT, TLS_TEST_MAGIC,
            "TLS var not initialized correctly: {:#x}",
            SOME_INT
        );
    }
}
