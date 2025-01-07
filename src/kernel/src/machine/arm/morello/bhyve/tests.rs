use core::ops::RangeInclusive;

use twizzler_abi::object::Protections;

use crate::{
    arch::memory::mmio::MMIO_ALLOCATOR,
    memory::{
        frame::PhysicalFrameFlags,
        pagetables::{ContiguousProvider, Mapper, MappingFlags, MappingSettings},
    },
};

/*
 * address mappings
 * first 4 GIB of lower address space has identity map
 * heap start: 0xFFFF_FF00_0000_0000
 *  - heap ranges from this address to 2 MiB up ...
 *  - until 0xffffff0000200000
 * kernel object mapping start: 0xFFFF_F000_0000_0000
 * - kernel obj mem ends at heap start
 */

const INVALID: [(u64, u64); 3] = [
    // heap
    (
        0xFFFF_FF00_0000_0000,
        0xFFFF_FF00_0000_0000 + 2 * 1024 * 1024,
    ),
    // kernel object
    (0xFFFF_F000_0000_0000, 0xFFFF_FF00_0000_0000),
    // first 4 gib
    (
        0xFFFF_0000_0000_0000,
        0xFFFF_0000_0000_0000 + 1024 * 1024 * 1024 * 4,
    ),
];

fn is_invalid(addr: u64) -> bool {
    for (a1, a2) in &INVALID {
        let range = core::ops::Range {
            start: *a1,
            end: *a2,
        };
        if range.contains(&addr) {
            return true;
        }
    }
    return false;
}

pub fn check_mmio_addrs() {
    // print memory map
    fn print_memory_map() {
        let current = unsafe { crate::memory::pagetables::Mapper::current() };
        let cursor = MappingCursor::new(VirtAddr::start_kernel_memory(), usize::MAX);
        for entry in current.readmap(cursor) {
            if entry.settings().cache() == CacheType::MemoryMappedIO || entry.paddr() == PHYS_ADDR {
                emerglogln!(
                    "{:?} => {:?} ({} KB, {:#x}) {:?}",
                    entry.vaddr(),
                    entry.paddr(),
                    entry.len() / 1024,
                    entry.len(),
                    entry.settings()
                );
            }
        }
    }

    emerglogln!("checking mmio validity ...");
    // The start range of valid addresses that TTBR1 covers
    const ADDR_START: u64 = 0xFFFF_0000_0000_0000;
    // The end range of valid addresses that TTBR1 covers
    // const ADDR_END: u64 = 0xFFFF_FFFF_FFFF_FFFF;
    // test physical address ...
    const PHYS_ADDR: PhysAddr = {
        use crate::machine::memory::BHYVE_UART;
        BHYVE_UART.start
    };
    const MAP_LEN: usize = crate::arch::memory::frame::FRAME_SIZE;
    // let mut offset: u64 = 0x1_0000_4000;
    // Device memory only prevetns speculative data accesses, so we must not
    // make this region executable to prevent speculative instruction accesses.
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        CacheType::MemoryMappedIO,
        MappingFlags::GLOBAL,
    );
    // The end range of valid addresses that TTBR1 covers
    // const ADDR_END: u64 = 0xFFFF_FFFF_FFFF_FFFF;
    // map the first x GB
    let addr_end = !0u64 - 4096;
    // let addr_end: u64 = ADDR_START + (6 * 1024 * 1024 * 1024);
    let mut addr = ADDR_START;
    let mut progress = 0;
    loop {
        if is_invalid(addr) {
            addr += MAP_LEN as u64;
            progress += 1;
            continue;
        }
        // let mmio_base = VirtAddr::new(ADDR_START + offset).expect("bad va");
        let mmio_base = unsafe { VirtAddr::new_unchecked(addr) }; //.expect("bad va");
                                                                  // map that sucker in ...
                                                                  // emerglogln!("trying to map in mmio: {:?} => {:#x} ({} KB, {:#x})",
                                                                  //     mmio_base, PHYS_ADDR, MAP_LEN / 1024, MAP_LEN
                                                                  // );
        let mut phys = ContiguousProvider::new(PHYS_ADDR, MAP_LEN);
        // configure mapping settings for this region of memory
        let cursor = MappingCursor::new(mmio_base, MAP_LEN);
        // map in with curent memory context
        unsafe {
            let mut mapper = Mapper::current();
            mapper.map(cursor, &mut phys, &settings);
        }
        // offset += MAP_LEN as u64;
        addr += MAP_LEN as u64;
        progress += 1;
        if progress % 10_000 == 0 {
            emerglogln!(
                "mapped in {} pages = {} MB",
                progress,
                progress * 4096 / 1024 / 1024
            );
            // if progress == 260000 {
            //     break;
            // }
            // print memory map
            emerglogln!("printing memory map ...");
            print_memory_map();
        }
        // if ADDR_START + offset == addr_end {
        if addr == addr_end {
            break;
        }
    }
}

pub fn test_map_mmio() {
    emerglogln!("testing mmio memory map");
    // this address seems random, but it exists ...
    // const PHYS_ADDR: usize = 0x2f13a0000;
    const PHYS_ADDR: PhysAddr = {
        use crate::machine::memory::BHYVE_UART;
        BHYVE_UART.start
    };
    const MAP_LEN: usize = crate::arch::memory::frame::FRAME_SIZE * 2;
    // use crate::arch::memory::mmio::MMIO_ALLOCATOR;
    // request 2 pages
    let mmio_base = {
        MMIO_ALLOCATOR
            .lock()
            .alloc(MAP_LEN)
            .expect("failed to allocate MMIO region")
    };
    // map that sucker in ...
    emerglogln!(
        "trying to map in mmio: {:?} => {:#x} ({} KB, {:#x})",
        mmio_base,
        PHYS_ADDR,
        MAP_LEN / 1024,
        MAP_LEN
    );
    // configure mapping settings for this region of memory
    let cursor = MappingCursor::new(mmio_base, MAP_LEN);
    let mut phys = ContiguousProvider::new(
        PHYS_ADDR, // unsafe { PhysAddr::new_unchecked(PHYS_ADDR as u64) },
        MAP_LEN,
    );
    // Device memory only prevetns speculative data accesses, so we must not
    // make this region executable to prevent speculative instruction accesses.
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        CacheType::MemoryMappedIO,
        MappingFlags::GLOBAL,
    );
    emerglogln!("doing the actual memory map");
    // map in with curent memory context
    unsafe {
        let mut mapper = Mapper::current();
        mapper.map(cursor, &mut phys, &settings);
    }
    emerglogln!("after the memory map");

    // print memory map
    let mut mmio_found = false;
    let current = unsafe { crate::memory::pagetables::Mapper::current() };
    let cursor = MappingCursor::new(VirtAddr::start_kernel_memory(), usize::MAX);
    for entry in current.readmap(cursor) {
        if entry.settings().cache() == CacheType::MemoryMappedIO || entry.paddr() == PHYS_ADDR {
            emerglogln!(
                "{:?} => {:?} ({} KB, {:#x}) {:?}",
                entry.vaddr(),
                entry.paddr(),
                entry.len() / 1024,
                entry.len(),
                entry.settings()
            );
            mmio_found = true;
        }
    }

    if !mmio_found {
        emerglogln!("did not find any mmio mappings!!");
    }
}

pub fn test_simple_map() {
    // this address seems random, but it exists ...
    emerglogln!("testing simple memory map");
    // const PHYS_ADDR: usize = 0x2f13a0000;
    // let frame = crate::memory::frame::alloc_frame(PhysicalFrameFlags::empty());
    // let addr = frame.start_address();
    let addr = {
        use crate::machine::memory::BHYVE_UART;
        BHYVE_UART.start
    };

    const MAP_LEN: usize = crate::arch::memory::frame::FRAME_SIZE;
    // request 2 pages
    let va_base = VirtAddr::new(0xFFFF_FFFF_FFFF_0000).unwrap();
    // {
    //     MMIO_ALLOCATOR.lock().alloc(MAP_LEN)
    //         .expect("failed to allocate MMIO region")
    // };
    // map that sucker in ...
    emerglogln!(
        "trying to map in page: {:?} => {:#x} ({} KB, {:#x})",
        va_base,
        addr.raw(),
        MAP_LEN / 1024,
        MAP_LEN
    );
    // configure mapping settings for this region of memory
    let cursor = MappingCursor::new(va_base, MAP_LEN);
    let mut phys = ContiguousProvider::new(addr, MAP_LEN);
    // Device memory only prevetns speculative data accesses, so we must not
    // make this region executable to prevent speculative instruction accesses.
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        // CacheType::WriteBack,
        CacheType::MemoryMappedIO,
        MappingFlags::GLOBAL,
    );
    // map in with curent memory context
    unsafe {
        let mut mapper = Mapper::current();
        mapper.map(cursor, &mut phys, &settings);
    }

    // print memory map
    let mut mapping_found = false;
    let current = unsafe { crate::memory::pagetables::Mapper::current() };
    let cursor = MappingCursor::new(VirtAddr::start_kernel_memory(), usize::MAX);
    for entry in current.readmap(cursor) {
        if entry.paddr() == addr {
            emerglogln!(
                "{:?} => {:?} ({} KB, {:#x}) {:?}",
                entry.vaddr(),
                entry.paddr(),
                entry.len() / 1024,
                entry.len(),
                entry.settings()
            );
            mapping_found = true;
        }
    }

    if !mapping_found {
        emerglogln!("did not find any mappings!!");
    }
}

pub fn print_limine_page_tables() {
    // print mair_el1
    emerglogln!("printing out limine page tables ...");
    let mut page_count = 0;
    let mut start_va = unsafe { VirtAddr::new_unchecked(0) };
    let mut start_pa = unsafe { PhysAddr::new_unchecked(0) };
    let mut page_end = 0;
    let mut found_base_page = false;
    let mut found_end_page = false;
    // print limine page tables
    let current = unsafe { crate::memory::pagetables::Mapper::current() };
    let cursor = MappingCursor::new(VirtAddr::start_kernel_memory(), usize::MAX);
    for entry in current.readmap(cursor) {
        // what is the end condition?
        // well when the page end range is != our predicted page end
        // and we are loking for a base page end
        if entry.vaddr().raw() != page_end && found_base_page {
            found_base_page = false;
            found_end_page = true;
        }

        if entry.settings().cache() == CacheType::MemoryMappedIO {
            emerglogln!(
                "{:?} => {:#x} ({} KB, {:#x}) {:?}",
                entry.vaddr(),
                entry.vaddr().raw() + entry.len() as u64,
                entry.len() / 1024,
                entry.len(),
                entry.settings()
            );
        }

        // if we found the end, then print out what we've got
        if found_end_page {
            let len = (page_count as usize * 4096) / 1024;
            emerglogln!("{:?} => {:?} ({} KB)", start_va, start_pa, len);
            found_end_page = false;
            page_end = 0;
            page_count = 0;
        }

        // we might still start a new 4k range so track that
        if entry.len() == 4096 {
            if found_base_page == false {
                start_va = entry.vaddr();
                start_pa = entry.paddr();
            }
            found_base_page = true;
            page_count += 1;
            page_end = entry.vaddr().raw() + entry.len() as u64;
        }

        // remeber, we are still on a new range, so print that one out too
        // but only do this if we have found the end page, or we are are not
        // looking for a page range
        if entry.len() != 4096 {
            // if found_base_page == false {
            emerglogln!(
                "{:?} => {:#x} ({} KB, {:#x}) {:?}",
                entry.vaddr(),
                entry.vaddr().raw() + entry.len() as u64,
                entry.len() / 1024,
                entry.len(),
                entry.settings()
            );
        }
    }
}

// custom DTB parsing, ugh ...
struct FdtHeader {
    _pad1: [u32; 2],
    off_struct: u32,
    _pad2: [u32; 7],
}

use twizzler_abi::device::CacheType;

// test to see if bootloader found the device tree
use crate::arch::BootInfoSystemTable;
pub fn check_device_tree<B: BootInfo>(boot_info: &B) -> bool {
    let found_dtb = {
        let bootloader_dtb_addr = boot_info.get_system_table(BootInfoSystemTable::Dtb);
        emerglogln!("dtb addr: {:#x}", bootloader_dtb_addr.raw());
        // we did not find it if this call returns a zero address
        bootloader_dtb_addr.raw() != 0
    };
    found_dtb
}

pub fn dump_dtb<B: BootInfo>(boot_info: &B) {
    if check_device_tree(boot_info) {
        let bootloader_dtb_addr = boot_info.get_system_table(BootInfoSystemTable::Dtb);
        use fdt::Fdt;
        let fdt = unsafe {
            Fdt::from_ptr(bootloader_dtb_addr.as_ptr()).expect("invalid DTB file, cannot boot")
        };
        let dtb_size = {
            // initialize the device tree
            let size = fdt.total_size();
            emerglogln!("dtb size: {:#x} ({})", size, size);
            if size > 4096 {
                4096
            } else {
                size
            }
        };
        let dtb_slice =
            unsafe { core::slice::from_raw_parts(bootloader_dtb_addr.as_ptr(), dtb_size) };
        use pretty_hex::PrettyHex;
        emerglogln!("{:?}", dtb_slice.hex_dump());
        emerglogln!("printing devictree: {:?}", fdt);

        // does all nodes return everything??
        for node in fdt.all_nodes() {
            emerglogln!("{}", node.name);
        }
    }
}

use crate::{
    memory::{pagetables::MappingCursor, PhysAddr, VirtAddr},
    BootInfo,
};
pub fn print_device_tree<B: BootInfo>(boot_info: &B) {
    emerglogln!("[test] looking at device tree");
    if check_device_tree(boot_info) {
        // initialize the device tree
        crate::machine::info::init(boot_info);
        let fdt = crate::machine::info::devicetree();
        emerglogln!("[test] initialized device tree! size: {}", fdt.total_size());
        // do something with the device tree
        if let Some(uart) = crate::machine::info::find_compatable("arm,pl011") {
            emerglogln!("found uart!: {}", uart.name);
        } else {
            emerglogln!("did not find uart");
        }

        if let Some(uart) = fdt.find_compatible(&["arm,pl011"]) {
            emerglogln!("lib found uart!: {}", uart.name);
        } else {
            emerglogln!("library did not find uart");
        }

        crate::machine::info::print_device_tree();
    } else {
        emerglogln!("[test] did not find device tree!");
    }
}

// print registers from uart
pub fn print_uart_config() {
    todo!()
}

// test causes an exception
pub fn test_exception_path() {
    emerglogln!("[test] testing exception path");
    // should end up in the debug handler

    // causes runtime exception
    // emerglogln!("[test] divide by zero");
    // #[allow(unconditional_panic)]
    // let _x = 1 / 0;

    // not a good test since it relies on current thread ref being set ...
    // let addr = 0xFFFF_FFFF_FFFF_1000 as u64;
    // emerglogln!("[test] accessing memory at address: {:#x}", addr);
    // // test another thing:
    // unsafe {
    //     // dereference the page
    //     let _y = *(addr as *mut u8);
    //     // write to it
    //     let _z = *(addr as *mut u8) = 0xff;
    // }

    emerglogln!("[test] invalid syscall");
    unsafe {
        core::arch::asm!("svc 21", options(nomem, nostack));
    }
    // does not return, oops ...
    emerglogln!("[test] returned!");

    // should not reach here
    // emerglogln!("should not reach here!");
    loop {}
    emerglogln!("should not reach here!");
}

pub struct Register {
    val: u64,
}

impl Register {
    pub fn new(val: u64) -> Self {
        Register { val }
    }

    pub fn bits(&self, bits: RangeInclusive<usize>) -> u64 {
        if *bits.start() == *bits.end() {
            let mask = 1 << *bits.end();
            (self.val & mask) >> *bits.start()
        } else {
            let mut mask = !0u64;
            let left_shift = 64 - *bits.end();
            let right_shift = *bits.start();
            mask <<= left_shift;
            mask >>= left_shift + right_shift;
            mask <<= right_shift;
            emerglogln!(
                "mask used for bits {}-{}: {:#x}",
                right_shift,
                *bits.end(),
                mask
            );
            (self.val & mask) >> right_shift
        }
    }

    pub fn get(&self, bit: usize) -> u64 {
        self.bits(bit..=bit)
    }

    pub fn raw(&self) -> u64 {
        self.val
    }
}

pub fn check_morello() {
    use arm64::registers::CPACR_EL1;
    use registers::interfaces::Readable;
    // check the bits in CPACR_EL1
    let cpacr = Register::new(CPACR_EL1.get());
    let cen = cpacr.bits(18..=19);
    emerglogln!("CPACR_EL1: {:#x} => CEN: 0b{:b}", cpacr.raw(), cen);
    // check the bits in SPSR_EL1
    // unsafe {
    //     core::arch::asm!(
    //         "svc 32",
    //     );
    // }
}

pub fn untrap_morello_inst() {
    use arm64::registers::CPACR_EL1;
    use registers::interfaces::{Readable, Writeable};
    // check the bits in CPACR_EL1
    let cpacr = CPACR_EL1.get();
    let cen = 0b11;
    CPACR_EL1.set(cpacr | cen << 18);
    // let cen = cpacr.bits(18..=19);
}
