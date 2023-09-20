use alloc::vec::Vec;
use limine::{
    BootInfoRequest, EntryPointRequest, File, FramebufferRequest,
    KernelFileRequest, MemoryMapEntryType, MemmapRequest, ModuleRequest,
    Ptr, RsdpRequest,
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
            VirtAddr::from_ptr(self.kernel.base.as_ptr().unwrap()),
            self.kernel.length as usize,
        )
    }

    fn get_system_table(&self, table: BootInfoSystemTable) -> VirtAddr {
        match table {
            BootInfoSystemTable::Rsdp => VirtAddr::new(self.rsdp.unwrap()).unwrap(),
            BootInfoSystemTable::Efi => todo!(),
        }
    }

    fn get_cmd_line(&self) -> &'static str {
        if let Some(cmd) = self.kernel.cmdline.as_ptr() {
            let ptr = cmd as *const u8;
            let slice = unsafe { core::slice::from_raw_parts(ptr, 0x1000) };
            let slice = &slice[0..slice.iter().position(|r| *r == 0).unwrap_or(0)];
            core::str::from_utf8(slice).unwrap()
        } else {
            ""
        }
    }
}

impl From<MemoryMapEntryType> for MemoryRegionKind {
    fn from(st: MemoryMapEntryType) -> Self {
        match st {
            MemoryMapEntryType::Usable => MemoryRegionKind::UsableRam,
            MemoryMapEntryType::KernelAndModules => MemoryRegionKind::BootloaderReserved,
            _ => MemoryRegionKind::Reserved,
        }
    }
}

const STACK_SIZE: usize = 4096 * 16;
#[repr(C, align(4096))]
struct P2Align12<T>(T);
static STACK: P2Align12<[u8; STACK_SIZE]> = P2Align12([0; STACK_SIZE]);

fn limine_entry() -> ! {
    unsafe {
        let efer = x86::msr::rdmsr(x86::msr::IA32_EFER);
        x86::msr::wrmsr(x86::msr::IA32_EFER, efer | (1 << 11));
        let cr4 = x86::controlregs::cr4();
        x86::controlregs::cr4_write(cr4 | x86::controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES);
        let cr0 = x86::controlregs::cr0();
        x86::controlregs::cr0_write(cr0 & !x86::controlregs::Cr0::CR0_WRITE_PROTECT);
    }

    LIMINE_BOOTINFO.get_response().get().unwrap();

    let mut boot_info = LimineBootInfo {
        kernel: unsafe {
            LIMINE_KERNEL
                .get_response()
                .get()
                .expect("no kernel info specified for kernel")
                .kernel_file
                .as_ptr()
                .unwrap()
                .as_ref()
                .unwrap()
        },
        maps: alloc::vec![],
        modules: alloc::vec![],
        rsdp: LIMINE_TABLE.get_response().get().map(|r| {
            r.address.as_ptr().unwrap() as u64 - 0xffff800000000000
        } /* TODO: MEGA HACK */),
    };

    boot_info.maps = LIMINE_MEM
        .get_response()
        .get()
        .expect("no memory map specified for kernel")
        .memmap()
        .iter()
        .map(|m| MemoryRegion {
            kind: m.typ.into(),
            start: PhysAddr::new(m.base).unwrap(),
            length: m.len as usize,
        })
        .collect();
    boot_info.modules = LIMINE_MOD
        .get_response()
        .get()
        .expect("no modules specified for kernel -- no way to start init")
        .modules()
        .iter()
        .map(|m| BootModule {
            start: VirtAddr::new(m.base.as_ptr().unwrap() as u64).unwrap(),
            length: m.length as usize,
        })
        .collect();
    crate::kernel_main(&mut boot_info);
}

static LIMINE_BOOTINFO: BootInfoRequest = BootInfoRequest::new(0);
static LIMINE_ENTRY: EntryPointRequest =
    EntryPointRequest::new(0).entry(Ptr::new(limine_entry));
static LIMINE_FB: FramebufferRequest = FramebufferRequest::new(0);
static LIMINE_MOD: ModuleRequest = ModuleRequest::new(0);
static LIMINE_MEM: MemmapRequest = MemmapRequest::new(0);
static LIMINE_KERNEL: KernelFileRequest = KernelFileRequest::new(0);
static LIMINE_TABLE: RsdpRequest = RsdpRequest::new(0);

#[link_section = ".limine_reqs"]
#[used]
static F1: &'static BootInfoRequest = &LIMINE_BOOTINFO;
#[link_section = ".limine_reqs"]
#[used]
static F2: &'static EntryPointRequest = &LIMINE_ENTRY;
#[link_section = ".limine_reqs"]
#[used]
static F3: &'static ModuleRequest = &LIMINE_MOD;
#[link_section = ".limine_reqs"]
#[used]
static F4: &'static MemmapRequest = &LIMINE_MEM;
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
static FEND: u64 = 0;
