use core::mem::size_of;

use x86::current::task::TaskStateSegment;
use x86::dtables::DescriptorTablePointer;
use x86::segmentation::BuildDescriptor;
use x86::segmentation::CodeSegmentType;
use x86::segmentation::DataSegmentType;
use x86::segmentation::Descriptor;
use x86::segmentation::DescriptorBuilder;
use x86::segmentation::SegmentDescriptorBuilder;
use x86::segmentation::SegmentSelector;
use x86::Ring;

use crate::memory::VirtAddr;

struct GlobalDescriptorTable {
    entries: [u64; 8],
    len: usize,
    code: SegmentSelector,
    tss: SegmentSelector,
}

impl GlobalDescriptorTable {
    fn new(tss: &'static TaskStateSegment) -> Self {
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

    fn get_user_selectors(&self) -> (u16, u16) {
        // Third entry in the GDT is the user data selector
        let user_data_sel = SegmentSelector::new(3, Ring::Ring3);
        // Forth entry in the GDT is the user code selector
        let user_code_sel = SegmentSelector::new(4, Ring::Ring3);
        (user_code_sel.index(), user_data_sel.index())
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
static mut STACK: [u128; STACK_SIZE / 16] = [0; STACK_SIZE / 16];

fn make_tss(stack: &'static [u128]) -> TaskStateSegment {
    let mut tss = TaskStateSegment::new();
    tss.set_ist(DOUBLE_FAULT_IST_INDEX as usize, {
        let stack_start = VirtAddr::from_ptr(stack.as_ptr());
        stack_start.offset(STACK_SIZE).unwrap().into()
    });
    tss
}

lazy_static::lazy_static! {
    static ref TSS: TaskStateSegment = {
        make_tss(unsafe {&STACK})
    };
}

lazy_static::lazy_static! {
    static ref GDT: GlobalDescriptorTable = {
        GlobalDescriptorTable::new(&TSS)
    };
}

pub fn init() {
    unsafe {
        GDT.load();
        x86::segmentation::load_cs(GDT.code);
        x86::task::load_tr(GDT.tss);
        x86::segmentation::load_ds(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_ss(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_gs(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_fs(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_es(SegmentSelector::new(0, Ring::Ring0));
    }
}

/// Get the user segment selectors. Returns (user-code-sel, user-data-sel).
pub(super) fn user_selectors() -> (u16, u16) {
    GDT.get_user_selectors()
}

#[thread_local]
static mut SGDT: Option<GlobalDescriptorTable> = None;
#[thread_local]
static mut STSS: Option<TaskStateSegment> = None;
#[thread_local]
static mut DF_STACK: [u128; STACK_SIZE / 16] = [0; STACK_SIZE / 16];

pub fn init_secondary() {
    unsafe {
        STSS = Some(make_tss(&DF_STACK));
        SGDT = Some(GlobalDescriptorTable::new(STSS.as_ref().unwrap()));
        SGDT.as_ref().unwrap().load();
        x86::segmentation::load_cs(SGDT.as_ref().unwrap().code);
        x86::task::load_tr(SGDT.as_ref().unwrap().tss);
        x86::segmentation::load_ds(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_ss(SegmentSelector::new(0, Ring::Ring0));
        //x86::segmentation::load_gs(SegmentSelector::new(0, Ring::Ring0));
        //x86::segmentation::load_fs(SegmentSelector::new(0, Ring::Ring0));
        x86::segmentation::load_es(SegmentSelector::new(0, Ring::Ring0));
    }
}

pub unsafe fn set_kernel_stack(stack: VirtAddr) {
    STSS.as_mut().unwrap().set_rsp(Ring::Ring0, stack.into());
    core::arch::asm!("mov gs:0, rax", in("rax") stack.raw());
    core::arch::asm!("mfence");
}
