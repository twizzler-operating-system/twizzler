use lazy_static::lazy_static;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

struct Selectors {
    code_sel: SegmentSelector,
    tss_sel: SegmentSelector,
}
const STACK_SIZE: usize = 0x1000 * 5;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            static mut STACK: [u128; STACK_SIZE / 16] = [0; STACK_SIZE / 16];

            let stack_start = VirtAddr::from_ptr(unsafe { &STACK });
            stack_start + STACK_SIZE
        };
        tss
    };
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_sel = gdt.add_entry(Descriptor::kernel_code_segment());
        gdt.add_entry(Descriptor::kernel_data_segment());
        gdt.add_entry(Descriptor::user_data_segment());
        gdt.add_entry(Descriptor::user_code_segment());
        let tss_sel = gdt.add_entry(Descriptor::tss_segment(&TSS));
        (gdt, Selectors { code_sel, tss_sel })
    };
}

pub fn init() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::registers::segmentation;
    use x86_64::registers::segmentation::Segment;
    GDT.0.load();
    unsafe {
        segmentation::CS::set_reg(GDT.1.code_sel);
        load_tss(GDT.1.tss_sel);
        segmentation::DS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        segmentation::SS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        segmentation::GS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        segmentation::FS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        segmentation::ES::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
    }
}

#[thread_local]
static mut SGDT: Option<(GlobalDescriptorTable, Selectors)> = None;
#[thread_local]
static mut STSS: Option<TaskStateSegment> = None;
#[thread_local]
static mut DF_STACK: [u128; STACK_SIZE / 16] = [0; STACK_SIZE / 16];

pub fn init_secondary() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::registers::segmentation;
    use x86_64::registers::segmentation::Segment;

    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        let stack_start = VirtAddr::from_ptr(unsafe { &DF_STACK });
        stack_start + STACK_SIZE
    };
    unsafe {
        STSS = Some(tss);
    }

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
        segmentation::DS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        segmentation::SS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        //segmentation::GS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        //segmentation::FS::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
        segmentation::ES::set_reg(SegmentSelector::new(0, x86_64::PrivilegeLevel::Ring0));
    }
}

pub unsafe fn set_kernel_stack(stack: VirtAddr) {
    STSS.as_mut().unwrap().privilege_stack_table[0] = stack;
    core::arch::asm!("mov gs:0, rax", in("rax") stack.as_u64());
    core::arch::asm!("mfence");
}
