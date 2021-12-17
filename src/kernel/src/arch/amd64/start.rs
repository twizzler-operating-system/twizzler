use alloc::vec::Vec;
use stivale_boot::v2::{
    StivaleFramebufferHeaderTag, StivaleHeader, StivaleMemoryMapEntryType, StivaleStruct,
    StivaleUnmapNullHeaderTag,
};
use x86_64::VirtAddr;

use crate::{
    memory::{MemoryRegion, MemoryRegionKind},
    BootInfo,
};

global_asm!(
    ".section .rodata",
    "mb2_hdr_start:",
    ".long 0x85250D6", //multiboot2 magic
    ".long 0",         //arch x86
    ".long mb2_hdr_end - mb2_hdr_start",
    ".long -(0xE85250D6 + 0 + (mb2_hdr_end - mb2_hdr_start))",
    "_mbh_info_start:",
    ".short 1",
    ".short 0",
    ".long _mbh_info_end - _mbh_info_start",
    ".long 3",  //module
    ".long 9",  //elf sec
    ".long 12", //efi64
    ".long 14", //acpi old
    ".long 15", //acpi new
    ".long 6",  //mmap
    "_mbh_info_end:",
    "_mbh_fb_start:",
    ".short 5",
    ".short 0",
    ".long _mbh_fb_end - _mbh_fb_start",
    ".long 0",
    ".long 0",
    ".long 32",
    ".long 0",
    "_mbh_fb_end:",
    ".short 0",
    ".short 0",
    ".long 8",
    "mb2_hdr_end:",
);

#[naked]
#[allow(named_asm_labels)]
#[export_name = "_start"]
pub unsafe extern "C" fn ____start() -> ! {
    asm!(
        "kernel_multiboot_entry: jmp kernel_multiboot_entry",
        ".align 8",
        options(noreturn)
    );
}

struct StivaleBootInfo {
    arch: &'static StivaleStruct,
    maps: Vec<MemoryRegion>,
}

pub enum BootInfoSystemTable {
    Rsdp,
    Efi,
}

impl BootInfo for StivaleBootInfo {
    fn memory_regions(&self) -> &'static [MemoryRegion] {
        unsafe { core::intrinsics::transmute(&self.maps[..]) }
    }

    fn kernel_image_info(&self) -> (VirtAddr, usize) {
        let info = self
            .arch
            .kernel_file_v2()
            .expect("failed to read kernel image from bootloader");
        (VirtAddr::new(info.kernel_start), info.kernel_size as usize)
    }

    fn get_system_table(&self, table: BootInfoSystemTable) -> VirtAddr {
        match table {
            BootInfoSystemTable::Rsdp => VirtAddr::new(
                self.arch
                    .rsdp()
                    .expect("failed to get RSDP from boot info")
                    .rsdp,
            ),
            BootInfoSystemTable::Efi => todo!(),
        }
    }
}

impl From<StivaleMemoryMapEntryType> for MemoryRegionKind {
    fn from(st: StivaleMemoryMapEntryType) -> Self {
        match st {
            StivaleMemoryMapEntryType::Usable => MemoryRegionKind::UsableRam,
            StivaleMemoryMapEntryType::BootloaderReclaimable => {
                MemoryRegionKind::BootloaderReserved
            }
            StivaleMemoryMapEntryType::Kernel => MemoryRegionKind::BootloaderReserved,
            _ => MemoryRegionKind::Reserved,
        }
    }
}

extern "C" fn __stivale_start(info: &'static StivaleStruct) -> ! {
    logln!("early print? {:p}", info);
    unsafe {
        let efer = x86::msr::rdmsr(x86::msr::IA32_EFER);
        x86::msr::wrmsr(x86::msr::IA32_EFER, efer | (1 << 11));
        let cr4 = x86::controlregs::cr4();
        x86::controlregs::cr4_write(cr4 | x86::controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES);
    }
    let mut boot_info = StivaleBootInfo {
        arch: info,
        maps: alloc::vec![],
    };
    boot_info.maps = info
        .memory_map()
        .expect("no memory map passed from bootloader")
        .iter()
        .map(|m| MemoryRegion {
            kind: m.entry_type().into(),
            start: VirtAddr::new(m.base),
            length: m.length as usize,
        })
        .collect();
    crate::kernel_main(&mut boot_info);
}

const STACK_SIZE: usize = 4096 * 16;
#[link_section = ".stivale2hdr"]
#[used]
#[no_mangle]
static STIVALE_HDR: StivaleHeader = StivaleHeader::new()
    .entry_point(__stivale_start)
    .stack(&STACK.0[STACK_SIZE - 4096] as *const u8)
    .tags((&FRAMEBUFFER_TAG as *const StivaleFramebufferHeaderTag).cast());

static UNMAP_NULL: StivaleUnmapNullHeaderTag = StivaleUnmapNullHeaderTag::new();

static FRAMEBUFFER_TAG: StivaleFramebufferHeaderTag = StivaleFramebufferHeaderTag::new()
    .framebuffer_bpp(24)
    .next((&UNMAP_NULL as *const StivaleUnmapNullHeaderTag).cast());

#[repr(C, align(4096))]
struct P2Align12<T>(T);
static STACK: P2Align12<[u8; STACK_SIZE]> = P2Align12([0; STACK_SIZE]);
