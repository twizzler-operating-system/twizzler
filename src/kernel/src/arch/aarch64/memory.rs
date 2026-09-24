use super::address::{PhysAddr, VirtAddr};

pub mod frame;
pub mod mmio;
pub mod pagetables;

/// The start offset into physical memory.
///
/// The kernel is designed to run in the higher
/// half of the virtual address space, and as such expects
/// a region of virtual memory to identity map
/// all physical memory. This is convenient since
/// calculating a physical to virtual address is simply
/// va = base + offset. Its value is set at early kernel
/// initialization and currently is bootloader-specific.
pub(super) static mut PHYS_MEM_OFFSET: u64 = 0;

// TODO: choose where our own identity map lives

// Kept in step with the amd64 version: an add and a range check on every identity-map access.
#[inline(always)]
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    VirtAddr::new(pa.raw() + unsafe { PHYS_MEM_OFFSET }).unwrap()
}

/// Who the translation is checked for: EL1 (`AT S1E1*`) or EL0 (`AT S1E0*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateAs {
    Kernel,
    User,
}

/// Ask the MMU to walk `va` through the live TTBR0/TTBR1 tables for a read or a write. `Err`
/// carries PAR_EL1.FST, which tells a missing mapping (translation fault) from a present but
/// badly encoded or insufficiently permissive one.
pub fn hw_translate(va: VirtAddr, write: bool, who: TranslateAs) -> Result<PhysAddr, u64> {
    let par: u64;
    unsafe {
        let old = super::interrupt::disable();
        match (who, write) {
            (TranslateAs::Kernel, false) => core::arch::asm!("at s1e1r, {}", in(reg) va.raw()),
            (TranslateAs::Kernel, true) => core::arch::asm!("at s1e1w, {}", in(reg) va.raw()),
            (TranslateAs::User, false) => core::arch::asm!("at s1e0r, {}", in(reg) va.raw()),
            (TranslateAs::User, true) => core::arch::asm!("at s1e0w, {}", in(reg) va.raw()),
        }
        core::arch::asm!("isb", "mrs {}, par_el1", out(reg) par);
        super::interrupt::set(old);
    }
    if par & 1 != 0 {
        return Err((par >> 1) & 0x3f);
    }
    Ok(PhysAddr::new((par & 0x0000_ffff_ffff_f000) | (va.raw() & 0xfff)).unwrap())
}

/// Assert the MMU's read walk agrees with `mapper`'s for up to `samples` pages spread over
/// `cursor`'s mappings; `mapper` must be the tables currently loaded for that range. Returns the
/// number of pages checked.
pub fn check_map(
    mapper: &crate::memory::pagetables::Mapper,
    cursor: crate::memory::pagetables::MappingCursor,
    samples: usize,
) -> usize {
    let who = if cursor.start().is_kernel() {
        TranslateAs::Kernel
    } else {
        TranslateAs::User
    };
    let page = frame::FRAME_SIZE;
    let stride = (cursor.remaining() / samples.max(1)).max(page) & !(page - 1);
    let mut checked = 0;
    for info in mapper.readmap(cursor) {
        if checked == samples {
            break;
        }
        let mut off = 0;
        while off < info.len() && checked < samples {
            let va = info.vaddr().offset(off).unwrap();
            assert_eq!(
                hw_translate(va, false, who),
                Ok(info.paddr().offset(off).unwrap()),
                "hardware walk of {:?} disagrees with {:?}",
                va,
                info
            );
            checked += 1;
            off += stride;
        }
    }
    checked
}

/// Inverse of [`phys_to_virt`] for addresses inside the identity map.
///
/// `None` for anything outside it, which is how a caller distinguishes a kernel-heap or
/// device-mapped address from a physical frame reached through the map.
pub fn virt_to_phys(va: VirtAddr) -> Option<PhysAddr> {
    let raw: u64 = va.raw();
    let off = unsafe { PHYS_MEM_OFFSET };
    PhysAddr::new(raw.checked_sub(off)?).ok()
}
