use lazy_static::lazy_static;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
use x86::current::task::TaskStateSegment;
use x86::segmentation::Descriptor;
use x86::segmentation::SegmentSelector;
use x86::Ring;

use crate::memory::VirtAddr;

struct Selectors {
    code_sel: SegmentSelector,
    tss_sel: SegmentSelector,
}
const STACK_SIZE: usize = 0x1000 * 5;

struct GlobalDescriptorTable {
    entries: [u64; 8],
    len: usize,
}

enum GdtEntry {
    Single(u64),
    Double(u64, u64),
}

impl GlobalDescriptorTable {
    const fn new() -> Self {
        Self {
            entries: [0, 0, 0, 0, 0, 0, 0, 0],
            len: 0,
        }
    }

    fn __do_add_entry(&mut self, entry: u64) -> usize {
        if self.len > 7 {
            panic!("increase GDT size");
        }
        self.entries[self.len] = entry.into();
        let r = self.len;
        self.len += 1;
        r
    }

    fn add_entry(&mut self, entry: GdtEntry) -> usize {
        match entry {
            GdtEntry::Single(x) => self.__do_add_entry(x),
            GdtEntry::Double(a, b) => {
                let r = self.__do_add_entry(a);
                self.__do_add_entry(b);
                r
            }
        }
    }

    fn load(&self) {
        todo!()
    }
}

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.set_ist(DOUBLE_FAULT_IST_INDEX as usize, {
            static mut STACK: [u128; STACK_SIZE / 16] = [0; STACK_SIZE / 16];

            let stack_start = VirtAddr::from_ptr(unsafe { &STACK });
            stack_start.offset(STACK_SIZE).unwrap().into()
        });
        tss
    };
}

use x86::segmentation::BuildDescriptor;
use x86::segmentation::CodeSegmentType;
use x86::segmentation::DescriptorBuilder;
use x86::segmentation::SegmentDescriptorBuilder;

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();

/*
        let code_sel = gdt.add_entry(Descriptor::kernel_code_segment());
        gdt.add_entry(Descriptor::kernel_data_segment());
        gdt.add_entry(Descriptor::user_data_segment());
        gdt.add_entry(Descriptor::user_code_segment());
        let tss_sel = gdt.add_entry(Descriptor::tss_segment(&TSS));
        (gdt, Selectors { code_sel, tss_sel })
         */
        todo!()
    };
}

pub fn init() {
    GDT.0.load();
    unsafe {
        todo!()
        /*
        segmentation::CS::set_reg(GDT.1.code_sel);
        load_tss(GDT.1.tss_sel);
        segmentation::DS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        segmentation::SS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        segmentation::GS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        segmentation::FS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        segmentation::ES::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        */
    }
}

#[thread_local]
static mut SGDT: Option<(GlobalDescriptorTable, Selectors)> = None;
#[thread_local]
static mut STSS: Option<TaskStateSegment> = None;
#[thread_local]
static mut DF_STACK: [u128; STACK_SIZE / 16] = [0; STACK_SIZE / 16];

pub fn init_secondary() {
    let mut tss = TaskStateSegment::new();
    tss.set_ist(DOUBLE_FAULT_IST_INDEX as usize, {
        let stack_start = VirtAddr::from_ptr(unsafe { &DF_STACK });
        stack_start.offset(STACK_SIZE).unwrap().into()
    });
    unsafe {
        STSS = Some(tss);
    }
    todo!()

    /*
    let mut gdt = GlobalDescriptorTable::new();
    let code_sel = gdt.add_entry(Descriptor::kernel_code_segment());
    gdt.add_entry(Descriptor::kernel_data_segment());
    gdt.add_entry(Descriptor::user_data_segment());
    gdt.add_entry(Descriptor::user_code_segment());
    unsafe {
        let tss_sel = gdt.add_entry(Descriptor::tss_segment(STSS.as_ref().unwrap()));
        SGDT = Some((gdt, Selectors { code_sel, tss_sel }));
        SGDT.as_ref().unwrap().0.load();
        segmentation::CS::set_reg(SGDT.as_ref().unwrap().1.code_sel);
        load_tss(SGDT.as_ref().unwrap().1.tss_sel);
        segmentation::DS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        segmentation::SS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        //segmentation::GS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        //segmentation::FS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        segmentation::ES::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
    }
    */
}

pub unsafe fn set_kernel_stack(stack: VirtAddr) {
    STSS.as_mut().unwrap().set_rsp(Ring::Ring0, stack.into());
    core::arch::asm!("mov gs:0, rax", in("rax") stack.raw());
    core::arch::asm!("mfence");
}
