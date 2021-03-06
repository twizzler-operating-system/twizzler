use core::{
    alloc::Layout,
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use crate::{once::Once, spinlock::Spinlock};
use alloc::{boxed::Box, collections::VecDeque, vec::Vec};
use x86_64::VirtAddr;

use crate::{
    arch::{self, processor::ArchProcessor},
    image::TlsInfo,
    sched::{CPUTopoNode, CPUTopoType},
    thread::{Priority, Thread, ThreadRef},
};

pub enum IPIDest {
    Single(u32),
    Others,
    All,
}
#[thread_local]
static mut BOOT_KERNEL_STACK: *mut u8 = core::ptr::null_mut();

#[thread_local]
static mut CPU_ID: u32 = 0;

#[thread_local]
static mut CURRENT_PROCESSOR: *const Processor = null_mut();

#[derive(Debug, Default)]
pub struct ProcessorStats {
    pub preempts: AtomicU64,
    pub wakeups: AtomicU64,
    pub steals: AtomicU64,
    pub idle: AtomicU64,
    pub non_idle: AtomicU64,
    pub hardticks: AtomicU64,
    pub switches: AtomicU64,
}

pub struct Processor {
    pub arch: ArchProcessor,
    pub sched: Spinlock<SchedulingQueues>,
    running: AtomicBool,
    topology_path: Once<Vec<(usize, bool)>>,
    pub id: u32,
    bsp_id: u32,
    pub idle_thread: Once<ThreadRef>,
    pub load: AtomicU64,
    pub stats: ProcessorStats,
}

const NR_QUEUES: usize = 32;
#[derive(Default)]
pub struct SchedulingQueues {
    pub queues: [VecDeque<ThreadRef>; NR_QUEUES],
    pub last_chosen_priority: Option<Priority>,
    exited: Vec<ThreadRef>,
}

impl SchedulingQueues {
    pub fn reinsert_thread(&mut self, thread: ThreadRef) -> bool {
        let queue_number = thread.queue_number::<NR_QUEUES>();
        let needs_preempt = if let Some(ref last) = self.last_chosen_priority {
            last < &thread.effective_priority()
        } else {
            false
        };
        self.queues[queue_number].push_back(thread);
        needs_preempt
    }

    pub fn check_priority_change(&mut self, thread: &Thread) -> bool {
        for i in 0..NR_QUEUES {
            let queue = &mut self.queues[i];
            for j in 0..queue.len() {
                if queue[j].id() == thread.id() {
                    let found = queue.remove(j).unwrap();
                    return self.reinsert_thread(found);
                }
            }
        }
        false
    }

    pub fn get_min_non_empty(&self) -> usize {
        for i in 0..NR_QUEUES {
            if !self.queues[i].is_empty() {
                return i;
            }
        }
        NR_QUEUES
    }

    pub fn has_work(&self) -> bool {
        self.get_min_non_empty() != NR_QUEUES || self.last_chosen_priority.is_some()
    }

    pub fn should_preempt(&self, pri: &Priority, eq: bool) -> bool {
        let q = pri.queue_number::<NR_QUEUES>();
        let m = self.get_min_non_empty();
        let c = self
            .last_chosen_priority
            .as_ref()
            .map_or(NR_QUEUES, |p| p.queue_number::<NR_QUEUES>());
        if eq {
            q <= m || q <= c
        } else {
            q < m || q < c
        }
    }

    pub fn has_higher_priority(&self, pri: Option<&Priority>) -> bool {
        let q = self.get_min_non_empty();
        if let Some(pri) = pri {
            let highest = Priority::from_queue_number::<NR_QUEUES>(q);
            &highest > pri
                || self
                    .last_chosen_priority
                    .as_ref()
                    .map_or(false, |last| last > pri)
        } else {
            q < NR_QUEUES || self.last_chosen_priority.is_some()
        }
    }

    pub fn choose_next(&mut self, for_self: bool) -> Option<ThreadRef> {
        for queue in &mut self.queues {
            if !queue.is_empty() {
                let choice = queue.pop_front();
                if for_self {
                    self.last_chosen_priority = choice.as_ref().map(|c| c.effective_priority());
                }
                return choice;
            }
        }
        if for_self {
            self.last_chosen_priority = None;
        }
        None
    }

    pub fn push_exited(&mut self, th: ThreadRef) {
        self.exited.push(th);
    }

    pub fn cleanup_exited(&mut self) {
        self.exited.clear();
    }
}

impl Processor {
    pub fn new(id: u32, bsp_id: u32) -> Self {
        Self {
            arch: ArchProcessor::default(),
            sched: Spinlock::new(Default::default()),
            running: AtomicBool::new(false),
            topology_path: Once::new(),
            id,
            bsp_id,
            idle_thread: Once::new(),
            load: AtomicU64::new(1),
            stats: ProcessorStats::default(),
        }
    }

    pub fn is_bsp(&self) -> bool {
        self.id == self.bsp_id
    }

    pub fn bsp_id(&self) -> u32 {
        self.bsp_id
    }

    pub fn current_priority(&self) -> Priority {
        /* TODO: optimize this by just keeping track of it outside the sched? */
        let sched = self.sched.lock();
        let queue_pri = Priority::from_queue_number::<NR_QUEUES>(sched.get_min_non_empty());
        if let Some(ref pri) = sched.last_chosen_priority {
            core::cmp::max(queue_pri, pri.clone())
        } else {
            queue_pri
        }
    }

    pub fn current_load(&self) -> u64 {
        self.load.load(Ordering::SeqCst)
    }

    fn set_topology(&self, topo_path: Vec<(usize, bool)>) {
        self.topology_path.call_once(|| topo_path);
    }

    fn set_running(&self) {
        self.running
            .store(true, core::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_idle_thread(&self, idle: ThreadRef) {
        self.idle_thread.call_once(|| idle);
    }
}

const MAX_CPU_ID: usize = 1024;

pub fn current_processor() -> &'static Processor {
    unsafe { CURRENT_PROCESSOR.as_ref() }.unwrap()
}

const INIT: Option<Box<Processor>> = None;
static mut ALL_PROCESSORS: [Option<Box<Processor>>; MAX_CPU_ID + 1] = [INIT; MAX_CPU_ID + 1];

pub fn get_processor(id: u32) -> &'static Processor {
    unsafe { ALL_PROCESSORS[id as usize].as_ref().unwrap() }
}

#[inline]
pub fn tls_ready() -> bool {
    crate::arch::processor::tls_ready()
}

pub const KERNEL_STACK_SIZE: usize = 81920;

const MIN_TLS_ALIGN: usize = 16;

fn init_tls(tls_template: TlsInfo) -> VirtAddr {
    let mut tls_size = tls_template.mem_size;
    let alignment = tls_template.align;

    let start_address_ptr = tls_template.start_addr.as_ptr();

    // The rhs of the below expression essentially calculates the amount of padding
    // we will have to introduce within the TLS region in order to achieve the desired
    // alignment.
    tls_size += (((!tls_size) + 1) - (start_address_ptr as usize)) & (alignment - 1);

    let tls_align = core::cmp::max(alignment, MIN_TLS_ALIGN);
    let full_tls_size = (core::mem::size_of::<*const u8>() + tls_size + tls_align + MIN_TLS_ALIGN
        - 1)
        & ((!MIN_TLS_ALIGN) + 1);

    let layout =
        Layout::from_size_align(full_tls_size, tls_align).expect("failed to unwrap TLS layout");

    let tls = unsafe {
        let tls = alloc::alloc::alloc_zeroed(layout);

        core::ptr::copy_nonoverlapping(start_address_ptr, tls, tls_template.file_size);

        tls
    };
    let tcb_base = VirtAddr::from_ptr(tls) + full_tls_size;

    unsafe { *(tcb_base.as_mut_ptr()) = tcb_base.as_u64() };

    tcb_base
}

pub fn init_cpu(tls_template: TlsInfo, bsp_id: u32) {
    let tcb_base = init_tls(tls_template);
    crate::arch::processor::init(tcb_base);
    unsafe {
        BOOT_KERNEL_STACK = 0xfffffff000001000u64 as *mut u8; //TODO: get this from bootloader config?
        CPU_ID = bsp_id;
        CURRENT_PROCESSOR = &**ALL_PROCESSORS[CPU_ID as usize].as_ref().unwrap();
    }
    let topo_path = arch::processor::get_topology();
    current_processor().set_topology(topo_path);
}

static CPU_MAIN_BARRIER: AtomicBool = AtomicBool::new(false);
pub fn secondary_entry(id: u32, tcb_base: VirtAddr, kernel_stack_base: *mut u8) -> ! {
    crate::arch::processor::init(tcb_base);
    arch::init_secondary();
    unsafe {
        BOOT_KERNEL_STACK = kernel_stack_base;
        CPU_ID = id;
        CURRENT_PROCESSOR = &**ALL_PROCESSORS[id as usize].as_ref().unwrap();
    }
    let topo_path = arch::processor::get_topology();
    current_processor().set_topology(topo_path);
    current_processor()
        .running
        .store(true, core::sync::atomic::Ordering::SeqCst);
    while !CPU_MAIN_BARRIER.load(core::sync::atomic::Ordering::SeqCst) {}
    crate::init_threading();
}

fn start_secondary_cpu(cpu: u32, tls_template: TlsInfo) {
    if cpu == 0 {
        panic!("TODO: we currently assume the bootstrap processor gets ID 0");
    }
    let tcb_base = init_tls(tls_template);
    /* TODO: dedicated kernel stack allocator, with guard page support */
    let kernel_stack = unsafe {
        let layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
        alloc::alloc::alloc_zeroed(layout)
    };

    //logln!("poking cpu {} {:?} {:?}", cpu, tcb_base, kernel_stack);
    unsafe {
        crate::arch::lapic::poke_cpu(cpu, tcb_base, kernel_stack);
    }
}

pub fn boot_all_secondaries(tls_template: TlsInfo) {
    for p in unsafe { &ALL_PROCESSORS }.iter().flatten() {
        if !p.running.load(core::sync::atomic::Ordering::SeqCst) {
            start_secondary_cpu(p.id, tls_template);
        }
        while !p.running.load(core::sync::atomic::Ordering::SeqCst) {}
    }

    let mut cpu_topo_root = CPUTopoNode::new(CPUTopoType::System);
    for p in unsafe { &ALL_PROCESSORS }.iter().flatten() {
        let topo_path = p.topology_path.wait();
        cpu_topo_root.set_cpu(p.id);
        let mut level = &mut cpu_topo_root;
        for (path, is_thread) in topo_path {
            let mut child = level.child_mut(*path);
            if child.is_none() {
                let ty = if *is_thread {
                    CPUTopoType::Thread
                } else {
                    CPUTopoType::Cache
                };
                level.add_child(*path, CPUTopoNode::new(ty));
                child = level.child_mut(*path);
            }

            let child = child.unwrap();

            child.set_cpu(p.id);

            let next = level.child_mut(*path);
            level = next.unwrap();
        }
    }
    crate::sched::set_cpu_topology(cpu_topo_root);
    crate::memory::finish_setup();
    CPU_MAIN_BARRIER.store(true, core::sync::atomic::Ordering::SeqCst);
}

pub fn register(id: u32, bsp_id: u32) {
    if id as usize >= unsafe { &ALL_PROCESSORS }.len() {
        unimplemented!("processor ID too large");
    }

    unsafe {
        ALL_PROCESSORS[id as usize] = Some(Box::new(Processor::new(id, bsp_id)));
        if id == bsp_id {
            ALL_PROCESSORS[id as usize].as_ref().unwrap().set_running();
        }
    }
}
