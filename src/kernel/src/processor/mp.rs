use alloc::boxed::Box;
use core::{
    alloc::Layout,
    cell::{RefCell, UnsafeCell},
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::{
    sched::{CPUTopoNode, CPUTopoType},
    tls_ready, Processor, KERNEL_STACK_SIZE,
};
use crate::{
    arch::{self, VirtAddr},
    image::TlsInfo,
};

#[thread_local]
static BOOT_KERNEL_STACK: RefCell<*mut u8> = RefCell::new(null_mut());

#[thread_local]
static CPU_ID: RefCell<u32> = RefCell::new(0);

#[thread_local]
static CURRENT_PROCESSOR: UnsafeCell<*const Processor> = UnsafeCell::new(null_mut());

pub fn init_cpu(tls_template: TlsInfo, bsp_id: u32) {
    let tcb_base = crate::arch::image::init_tls(tls_template);
    crate::arch::processor::init(tcb_base);
    unsafe {
        *BOOT_KERNEL_STACK.borrow_mut() = 0xfffffff000001000u64 as *mut u8; //TODO: get this from bootloader config?
        *CPU_ID.borrow_mut() = bsp_id;
        *CURRENT_PROCESSOR.get() = &**ALL_PROCESSORS[*CPU_ID.borrow() as usize].as_ref().unwrap();
    }
    let topo_path = arch::processor::get_topology();
    current_processor().set_topology(topo_path);
}

pub static NR_CPUS: AtomicUsize = AtomicUsize::new(1);

static CPU_MAIN_BARRIER: AtomicBool = AtomicBool::new(false);

pub fn secondary_entry(id: u32, tcb_base: VirtAddr, kernel_stack_base: *mut u8) -> ! {
    crate::arch::processor::init(tcb_base);
    unsafe {
        *BOOT_KERNEL_STACK.borrow_mut() = kernel_stack_base;
        *CPU_ID.borrow_mut() = id;
        *CURRENT_PROCESSOR.get() = &**ALL_PROCESSORS[id as usize].as_ref().unwrap();
    }
    arch::init_secondary();
    let topo_path = arch::processor::get_topology();
    current_processor().set_topology(topo_path);
    current_processor()
        .running
        .store(true, core::sync::atomic::Ordering::SeqCst);
    NR_CPUS.fetch_add(1, Ordering::SeqCst);
    while !CPU_MAIN_BARRIER.load(core::sync::atomic::Ordering::SeqCst) {}
    crate::init_threading();
}

fn start_secondary_cpu(cpu: u32, tls_template: TlsInfo) {
    if cpu == 0 {
        panic!("TODO: we currently assume the bootstrap processor gets ID 0");
    }
    let tcb_base = crate::arch::image::init_tls(tls_template);
    /* TODO: dedicated kernel stack allocator, with guard page support */
    let kernel_stack = unsafe {
        let layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
        alloc::alloc::alloc_zeroed(layout)
    };

    //logln!("poking cpu {} {:?} {:?}", cpu, tcb_base, kernel_stack);
    unsafe {
        crate::arch::poke_cpu(cpu, tcb_base, kernel_stack);
    }
}

pub fn boot_all_secondaries(tls_template: TlsInfo) {
    for p in all_processors().iter().flatten() {
        if !p.running.load(core::sync::atomic::Ordering::SeqCst) {
            start_secondary_cpu(p.id, tls_template);
        }
        while !p.running.load(core::sync::atomic::Ordering::SeqCst) {
            // We can safely spin-loop here because we are in kernel initialization.
            core::hint::spin_loop();
        }
    }

    let mut cpu_topo_root = Box::new(CPUTopoNode::new(CPUTopoType::System));
    for p in all_processors().iter().flatten() {
        let topo_path = p.topology_path.wait();
        cpu_topo_root.set_cpu(p.id);
        let mut level = &mut *cpu_topo_root;
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
    crate::processor::sched::set_cpu_topology(cpu_topo_root);
    CPU_MAIN_BARRIER.store(true, core::sync::atomic::Ordering::SeqCst);
    crate::memory::prep_smp();
}

pub fn register(id: u32, bsp_id: u32) {
    if id as usize >= all_processors().len() {
        log::warn!("processor ID {} not supported (too large)", id);
        return;
    }

    unsafe {
        ALL_PROCESSORS[id as usize] = Some(Box::new(Processor::new(id, bsp_id)));
        if id == bsp_id {
            ALL_PROCESSORS[id as usize].as_ref().unwrap().set_running();
        }
    }
}

pub const MAX_CPU_ID: usize = 1024;

pub fn current_processor() -> &'static Processor {
    if !tls_ready() {
        panic!("tried to read a thread-local value with no FS base set");
    }
    unsafe {
        CURRENT_PROCESSOR
            .get()
            .as_ref()
            .unwrap_unchecked()
            .as_ref()
            .unwrap_unchecked()
    }
}

const INIT: Option<Box<Processor>> = None;
static mut ALL_PROCESSORS: [Option<Box<Processor>>; MAX_CPU_ID + 1] = [INIT; MAX_CPU_ID + 1];

pub fn all_processors() -> &'static [Option<Box<Processor>>; MAX_CPU_ID + 1] {
    unsafe {
        #[allow(static_mut_refs)]
        &ALL_PROCESSORS
    }
}

pub fn get_processor(id: u32) -> &'static Processor {
    unsafe { ALL_PROCESSORS[id as usize].as_ref().unwrap() }
}

/// Obtain a mutable reference to a processor object. This should not be called unless
/// you know what you are doing. Generally during the boostrap process.
pub unsafe fn get_processor_mut(id: u32) -> &'static mut Processor {
    ALL_PROCESSORS[id as usize].as_mut().unwrap()
}

pub fn with_each_active_processor(mut f: impl FnMut(&'static Processor)) {
    for p in all_processors() {
        if let Some(p) = p {
            if p.is_running() {
                f(p)
            }
        }
    }
}
