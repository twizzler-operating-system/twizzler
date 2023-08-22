use crate::{terminal, term};

use twizzler_abi::{device::CacheType, object::Protections};

use super::super::uart::PL011;        
use super::memory::mmio::PL011_UART;

use crate::memory::{PhysAddr, VirtAddr, pagetables::{
    ContiguousProvider, MappingCursor, MappingSettings, Mapper,
    MappingFlags, Table,
}};

use crate::arch::memory::pagetables::{EntryFlags, Entry};

use crate::arch::memory::frame::FRAME_SIZE;
use crate::memory::frame::{alloc_frame, PhysicalFrameFlags};

// test serial output using limine's page tables and init alloc memory
//
// we need at least the page frame allocator initialized
// since creating a new memory mapping may in of itself allocate
// pages of memory
pub fn test_limine_serial() {

    // the desired virtal address for this region of mmio
    // make sure this address is different than other tests
    let uart_mmio_base = VirtAddr::new(0xFFFF_0000_0000_2000).unwrap();
    // configure mapping settings for this region of memory
    let cursor = MappingCursor::new(
        uart_mmio_base,
        PL011_UART.length,
    );
    let mut phys = ContiguousProvider::new(
        PL011_UART.start,
        PL011_UART.length,
    );
    // Device memory only prevents speculative data accesses, so we must not
    // make this region executable to prevent speculative instruction accesses.
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        CacheType::MemoryMappedIO,
        MappingFlags::GLOBAL,
    );
    terminal!("[kernel::test] mapping UART ...");
    // map in with curent memory context
    unsafe {
        let mut mapper = Mapper::current();
        mapper.map(cursor, &mut phys, &settings);
    }
    terminal!("[kernel::test] UART init ...");
    // create instance of the PL011 UART driver
    let serial_port = unsafe { 
        PL011::new(uart_mmio_base.into()) 
    };

    // so my initalization is off (on real hardware)
    // but my mapping code is good, and my printing is fine too!!!
    // serial_port.early_init();
    
    terminal!("[kernel::test] writing to UART ...");
    serial_port.tx_byte(b'A');
    serial_port.write_str("Hello World!\r\n");
    serial_port.tx_byte(b'B');
}

// print an entire page table, skipping empty entries and some valid entries
pub fn print_page_table(root: PhysAddr, level: usize) {
    terminal!("[kernel::tests] printing level {} page table rooted at: {:#018x}", 
        level,
        root.raw(),
    );
    let table = unsafe { 
        &*(root
            .kernel_vaddr()
            .as_ptr::<Table>()
        ) 
    };
    // iterate over entries and then print them out
    let mut count = 0;
    const MAX_COUNT: usize = 4;
    let mut pt_roots: [u64; Table::PAGE_TABLE_ENTRIES] = [0; Table::PAGE_TABLE_ENTRIES];
    
    let mut print_dash = 0;
    let mut skip_message = false;
    // let mut skip_entries = false;
    let mut present_count = 0;
    let mut present_skip = false;

    for idx in 0..Table::PAGE_TABLE_ENTRIES {
        // every other one, print some start message
        if count == 0 {
            if skip_message == false && !present_skip {
                term!("\t{:#018x}:", root.raw() + idx as u64);
            }

            if print_dash >= MAX_COUNT {
                terminal!("\t\t-- skipping empty entries --");
                skip_message = true;
                print_dash = 0;
            }
            if present_count >= MAX_COUNT * 2 {
                terminal!("\t\t** skipping some present entries **");
                present_count = 0;
                present_skip = true;
            }
        }
        count += 1;
        let entry = &table[idx];
        if entry.is_present() {
            if skip_message == true {
                term!("\t{:#018x}:", root.raw() + (idx - count + 1) as u64);
                for _ in 0..count-1 {
                    term!(" {}", "-".repeat(18));
                }
                present_count = 0;
            }
            pt_roots[idx] = entry.raw();
            if present_skip == false {
                term!(" {:#018x}", entry.raw());
                skip_message = false;
                present_count += 1;
            }
        } else {
            if present_skip == true {
                term!("\t{:#018x}:", root.raw() + idx as u64);
            }
            if skip_message == false {
                term!(" {}", "-".repeat(18));
                print_dash += 1;
                present_count = 0;
                present_skip = false;
            }
        }
        
        if count % MAX_COUNT == 0 {
            if skip_message == false && present_skip == false {
                term!("\n");
            }
            count = 0;   
        }
    }

    // print pt roots
    let mut huge_count = 0;
    let mut leaf_count = 0;
    for (i, &r) in pt_roots.iter().enumerate() {
        if r != 0 {
            let (e, is_table, huge) = if level != Table::last_level() {
                let e = &table[i];
                (*e, !e.is_huge(), e.is_huge())
            } else {
                (Entry::new_unused(), false, false)
            };
            if is_table {
                terminal!("==> entry found: {:#018x} table: {}, huge: {}", r, is_table, huge);
            }
            if huge {
                huge_count += 1;
            }
            if !huge && !is_table {
                leaf_count += 1;
            }
            if is_table {
                let next_table = e.table_addr();
                // let addr = entry.table_addr().kernel_vaddr();
                // unsafe { Some(&*(addr.as_ptr::<Table>())) }
                // table.next_table(i).unwrap();
                print_page_table(next_table, Table::next_level(level));
            }
        }
    }
//     terminal!("STATS: level {} table {:#018x} huge page count: {}, leaf count: {}", 
//         level,
//         root.raw(),
//         huge_count,
//         leaf_count,
//     );
}

pub fn test_terminal(x: u64) {
    // write a message to the terminal
    terminal!("Hello Int: {:#x}", x);
}

// test mapping in a page using limine's page tables
//
// we need at least the page frame allocator initialized
// since creating a new memory mapping may in of itself allocate
// pages of memory
pub fn test_map_limine() {
    // the desired virtal address for this page of memory
    const TARGET_ADDR: u64 = 0xFFFF_0000_0000_0000;
    let target_va = VirtAddr::new(TARGET_ADDR).unwrap();
    // configure mapping settings for this region of memory
    let cursor = MappingCursor::new(
        target_va,
        FRAME_SIZE,
    );
    terminal!("[kernel::test] allocating a page frame"); 
    // crate::memory::frame::init(boot_info.memory_regions());
    // allocate a physical frame to back this region of memory
    let target_pa = alloc_frame(PhysicalFrameFlags::ZEROED).start_address();
    // set the physical frame providor to give a contigiuous memory region
    let mut phys = ContiguousProvider::new(
        target_pa,
        FRAME_SIZE,
    );
    // map in the memory as r/w and kernel accessible
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        CacheType::WriteBack,
        MappingFlags::GLOBAL,
    );
    terminal!("[kernel::test] mapping in a page {:#018x} => {:#018x}", 
        TARGET_ADDR, target_pa.raw()
    );
    // map in with curent memory context (TTBR1_EL1)
    unsafe {
        let mut mapper = Mapper::current();
        mapper.map(cursor, &mut phys, &settings);
    }
    
    // test writing to that place in memory
    const TEST_VAL: u64 = 0xAA42_D00D;
    terminal!("[kernel::test] writing {:#x} to that page", TEST_VAL);
    unsafe {
        let page_ptr: *mut u64 = target_va.as_mut_ptr();
        *page_ptr = TEST_VAL;
    }
    // test reading from that place in memory
    terminal!("[kernel::test] reading from memory address {:#018x}", TARGET_ADDR);
    terminal!("[kernel::test] value of {:#018x} = {:#x}", TARGET_ADDR,
        unsafe {
            let page_ptr: *const u64 = target_va.as_ptr();
            *page_ptr
        }
    );
}
