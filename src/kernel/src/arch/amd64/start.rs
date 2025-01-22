use alloc::vec::Vec;

use limine::{
    file::File,
    memory_map::EntryType,
    request::{
        BootloaderInfoRequest, EntryPointRequest, FramebufferRequest, HhdmRequest,
        KernelFileRequest, MemoryMapRequest, ModuleRequest, RsdpRequest,
    },
    BaseRevision,
};

use crate::{
    initrd::BootModule,
    memory::{MemoryRegion, MemoryRegionKind, PhysAddr, VirtAddr},
    BootInfo,
};

struct LimineBootInfo {
    kernel: &'static File,
    maps: Vec<MemoryRegion>,
    modules: Vec<BootModule>,
    rsdp: Option<u64>,
}

pub enum BootInfoSystemTable {
    Rsdp,
    Efi,
}

impl BootInfo for LimineBootInfo {
    fn memory_regions(&self) -> &'static [MemoryRegion] {
        unsafe { core::intrinsics::transmute(&self.maps[..]) }
    }

    fn get_modules(&self) -> &'static [BootModule] {
        unsafe { core::intrinsics::transmute(&self.modules[..]) }
    }

    fn kernel_image_info(&self) -> (VirtAddr, usize) {
        (
            VirtAddr::from_ptr(self.kernel.addr()),
            self.kernel.size() as usize,
        )
    }

    fn get_system_table(&self, table: BootInfoSystemTable) -> VirtAddr {
        match table {
            BootInfoSystemTable::Rsdp => VirtAddr::new(self.rsdp.unwrap()).unwrap(),
            BootInfoSystemTable::Efi => todo!(),
        }
    }

    fn get_cmd_line(&self) -> &'static str {
        if !self.kernel.cmdline().is_empty() {
            core::str::from_utf8(self.kernel.cmdline()).unwrap()
        } else {
            ""
        }
    }
}

impl From<EntryType> for MemoryRegionKind {
    fn from(st: EntryType) -> Self {
        match st {
            EntryType::USABLE => MemoryRegionKind::UsableRam,
            EntryType::KERNEL_AND_MODULES => MemoryRegionKind::BootloaderReserved,
            _ => MemoryRegionKind::Reserved,
        }
    }
}

const STACK_SIZE: usize = 4096 * 16;
#[repr(C, align(4096))]
struct P2Align12<T>(T);
static STACK: P2Align12<[u8; STACK_SIZE]> = P2Align12([0; STACK_SIZE]);

extern "C" fn limine_entry() -> ! {
    unsafe {
        let efer = x86::msr::rdmsr(x86::msr::IA32_EFER);
        x86::msr::wrmsr(x86::msr::IA32_EFER, efer | (1 << 11));
        let cr4 = x86::controlregs::cr4();
        x86::controlregs::cr4_write(cr4 | x86::controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES);
        let cr0 = x86::controlregs::cr0();
        x86::controlregs::cr0_write(cr0 & !x86::controlregs::Cr0::CR0_WRITE_PROTECT);
    }

    LIMINE_BOOTINFO.get_response().unwrap();

    // Set the identity map offset used for fast physical to virtual translations.
    // The offset is only initialized once at startup so it is safe to write directly.
    let hhdm_info = LIMINE_HHDM
        .get_response()
        .expect("failed to get higher half direct ");
    unsafe {
        super::memory::PHYS_MEM_OFFSET = hhdm_info.offset();
    }

    let mut boot_info = LimineBootInfo {
        kernel: LIMINE_KERNEL
            .get_response()
            .expect("no kernel info specified for kernel")
            .file(),
        maps: alloc::vec![],
        modules: alloc::vec![],
        rsdp: LIMINE_TABLE.get_response().map(
            |r| r.address() as u64 - 0xffff800000000000, /* TODO: MEGA HACK */
        ),
    };

    boot_info.maps = LIMINE_MEM
        .get_response()
        .expect("no memory map specified for kernel")
        .entries()
        .iter()
        .map(|m| MemoryRegion {
            kind: m.entry_type.into(),
            start: PhysAddr::new(m.base).unwrap(),
            length: m.length as usize,
        })
        .collect();
    boot_info.modules = LIMINE_MOD
        .get_response()
        .expect("no modules specified for kernel -- no way to start init")
        .modules()
        .iter()
        .map(|m| BootModule {
            start: VirtAddr::from_ptr(m.addr()),
            length: m.size() as usize,
        })
        .collect();
    crate::kernel_main(&mut boot_info);
}

#[used]
#[link_section = ".limine_reqs"]
static LIMINE_REVISION: BaseRevision = BaseRevision::new();
static LIMINE_BOOTINFO: BootloaderInfoRequest = BootloaderInfoRequest::new();
static LIMINE_ENTRY: EntryPointRequest = EntryPointRequest::new().with_entry_point(limine_entry);
static LIMINE_FB: FramebufferRequest = FramebufferRequest::new();
static LIMINE_MOD: ModuleRequest = ModuleRequest::new();
static LIMINE_MEM: MemoryMapRequest = MemoryMapRequest::new();
static LIMINE_KERNEL: KernelFileRequest = KernelFileRequest::new();
static LIMINE_TABLE: RsdpRequest = RsdpRequest::new();
static LIMINE_HHDM: HhdmRequest = HhdmRequest::new();

#[link_section = ".limine_reqs"]
#[used]
static F1: &'static BootloaderInfoRequest = &LIMINE_BOOTINFO;
#[link_section = ".limine_reqs"]
#[used]
static F2: &'static EntryPointRequest = &LIMINE_ENTRY;
#[link_section = ".limine_reqs"]
#[used]
static F3: &'static ModuleRequest = &LIMINE_MOD;
#[link_section = ".limine_reqs"]
#[used]
static F4: &'static MemoryMapRequest = &LIMINE_MEM;
#[link_section = ".limine_reqs"]
#[used]
static F5: &'static KernelFileRequest = &LIMINE_KERNEL;
#[link_section = ".limine_reqs"]
#[used]
static F6: &'static FramebufferRequest = &LIMINE_FB;
#[link_section = ".limine_reqs"]
#[used]
static F7: &'static RsdpRequest = &LIMINE_TABLE;
#[link_section = ".limine_reqs"]
#[used]
static F8: &'static HhdmRequest = &LIMINE_HHDM;
#[link_section = ".limine_reqs"]
#[used]
static FEND: u64 = 0;
