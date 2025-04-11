use core::{cell::RefCell, mem::size_of};

use x86::{
    current::task::TaskStateSegment,
    dtables::DescriptorTablePointer,
    segmentation::{
        BuildDescriptor, CodeSegmentType, DataSegmentType, Descriptor, DescriptorBuilder,
        SegmentDescriptorBuilder, SegmentSelector,
    },
    Ring,
};

use crate::{memory::VirtAddr, once::Once};

struct GlobalDescriptorTable {
    entries: [u64; 8],
    len: usize,
    code: SegmentSelector,
    tss: SegmentSelector,
}

impl GlobalDescriptorTable {
    fn new(tss: *const TaskStateSegment) -> Self {
        let kernel_code64: Descriptor =
            DescriptorBuilder::code_descriptor(0, 0xffffffff, CodeSegmentType::ExecuteReadAccessed)
                .present()
                .dpl(Ring::Ring0)
                .limit_granularity_4kb()
                .l()
                .finish();
        let user_code64: Descriptor =
            DescriptorBuilder::code_descriptor(0, 0xffffffff, CodeSegmentType::ExecuteReadAccessed)
                .present()
                .dpl(Ring::Ring3)
                .limit_granularity_4kb()
                .l()
                .finish();
        let kernel_data: Descriptor =
            DescriptorBuilder::data_descriptor(0, 0xffffffff, DataSegmentType::ReadWriteAccessed)
                .present()
                .dpl(Ring::Ring0)
                .limit_granularity_4kb()
                .finish();
        let user_data: Descriptor =
            DescriptorBuilder::data_descriptor(0, 0xffffffff, DataSegmentType::ReadWriteAccessed)
                .present()
                .dpl(Ring::Ring3)
                .limit_granularity_4kb()
                .finish();

        let ptr = tss as *const _ as u64;

        let mut tss_desc = Descriptor::default();
        tss_desc.set_base_limit(ptr as u32, (size_of::<TaskStateSegment>() - 1) as u32);
        tss_desc.set_type(0b1001);
        tss_desc.set_p();
        tss_desc.set_l();
        let tss_desc_high = ptr >> 32;

        Self {
            entries: [
                0,
                kernel_code64.as_u64(),
                kernel_data.as_u64(),
                user_data.as_u64(),
                user_code64.as_u64(),
                tss_desc.as_u64(),
                tss_desc_high,
                0,
            ],
            len: 7,
            code: SegmentSelector::new(1, Ring::Ring0),
            tss: SegmentSelector::new(5, Ring::Ring0),
        }
    }

    fn get_user_selectors(&self) -> (SegmentSelector, SegmentSelector) {
        // Third entry in the GDT is the user data selector
        let user_data_sel = SegmentSelector::new(3, Ring::Ring3);
        // Forth entry in the GDT is the user code selector
        let user_code_sel = SegmentSelector::new(4, Ring::Ring3);
        (user_code_sel, user_data_sel)
    }

    fn __do_add_entry(&mut self, entry: u64) -> usize {
        if self.len > 7 {
            panic!("increase GDT size");
        }
        self.entries[self.len] = entry;
        let r = self.len;
        self.len += 1;
        r
    }

    unsafe fn load(&self) {
        let p = DescriptorTablePointer::new_from_slice(&self.entries[0..self.len]);
        x86::dtables::lgdt(&p);
    }
}

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
const STACK_SIZE: usize = 0x1000 * 5;

fn make_tss(stack: *const u128) -> TaskStateSegment {
    let mut tss = TaskStateSegment::new();
    tss.set_ist(DOUBLE_FAULT_IST_INDEX as usize, {
        let stack_start = VirtAddr::from_ptr(stack);
        stack_start.offset(STACK_SIZE).unwrap().into()
    });
    tss
}

static STACK: [u128; STACK_SIZE / 16] = [0; STACK_SIZE / 16];
static TSS: Once<TaskStateSegment> = Once::new();
static GDT: Once<GlobalDescriptorTable> = Once::new();

fn get_gdt() -> &'static GlobalDescriptorTable {
    GDT.call_once(|| GlobalDescriptorTable::new(get_tss()))
}

fn get_tss() -> &'static TaskStateSegment {
    TSS.call_once(|| make_tss(STACK.as_ptr()))
}

pub fn init() {
    unsafe {
        get_gdt().load();
        x86::segmentation::load_cs(get_gdt().code);

        x86::task::load_tr(get_gdt().tss);

        x86::segmentation::load_ds(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_ss(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_gs(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_fs(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_es(SegmentSelector::new(0, Ring::Ring0));
    }
}

/// Get the user segment selectors. Returns (user-code-sel, user-data-sel).
pub(super) fn user_selectors() -> (u16, u16) {
    let (code, data) = get_gdt().get_user_selectors();
    (code.bits(), data.bits())
}

#[thread_local]
static SGDT: RefCell<Option<GlobalDescriptorTable>> = RefCell::new(None);
#[thread_local]
static STSS: RefCell<Option<TaskStateSegment>> = RefCell::new(None);
#[thread_local]
static DF_STACK: RefCell<[u128; STACK_SIZE / 16]> = RefCell::new([0; STACK_SIZE / 16]);

pub fn init_secondary() {
    unsafe {
        let stack = DF_STACK.borrow();
        *STSS.borrow_mut() = Some(make_tss(stack.as_ptr()));
        *SGDT.borrow_mut() = Some(GlobalDescriptorTable::new(STSS.borrow().as_ref().unwrap()));
        SGDT.borrow().as_ref().unwrap().load();
        x86::segmentation::load_cs(SGDT.borrow().as_ref().unwrap().code);
        x86::task::load_tr(SGDT.borrow().as_ref().unwrap().tss);
        x86::segmentation::load_ds(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_ss(SegmentSelector::new(0, Ring::Ring0));
        //x86::segmentation::load_gs(SegmentSelector::new(0, Ring::Ring0));
        //x86::segmentation::load_fs(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_es(SegmentSelector::new(0, Ring::Ring0));
    }
}

pub unsafe fn set_kernel_stack(stack: VirtAddr) {
    STSS.borrow_mut()
        .as_mut()
        .unwrap()
        .set_rsp(Ring::Ring0, stack.into());
    core::arch::asm!("mov gs:0, rax", in("rax") stack.raw());
    core::arch::asm!("mfence");
}
