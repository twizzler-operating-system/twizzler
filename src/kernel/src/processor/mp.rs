use alloc::boxed::Box;
use core::{
    cell::{RefCell, UnsafeCell},
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::{
    Processor,
    sched::{CPUTopoNode, CPUTopoType},
    tls_ready,
    topology::TopoPath,
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
    //TODO: get the stack from bootloader config?
    init_cpu_locals(bsp_id, 0xfffffff000001000u64 as *mut u8);
}

/// Everything that touches a thread-local after the thread pointer is set. Kept out of the caller
/// because LLVM treats the thread pointer as invariant within a function and hoists its read above
/// the write (seen on aarch64: `mrs tpidr_el1` scheduled before the `msr`).
#[inline(never)]
fn init_cpu_locals(id: u32, kernel_stack_base: *mut u8) {
    unsafe {
        *BOOT_KERNEL_STACK.borrow_mut() = kernel_stack_base;
        *CPU_ID.borrow_mut() = id;
        *CURRENT_PROCESSOR.get() = &**ALL_PROCESSORS[id as usize].as_ref().unwrap();
    }
    let topo_path = arch::processor::get_topology();
    current_processor().set_topology(topo_path);
}

pub static NR_CPUS: AtomicUsize = AtomicUsize::new(1);

/// Highest processor id ever passed to [register], so [with_each_active_processor] can stop there
/// instead of sweeping all `MAX_CPU_ID + 1` slots to find the handful that exist.
///
/// A *max*, not a count: these are APIC ids ([`arch::processor`]'s two `register` calls pass
/// `local_apic_id`), which are not dense.
///
/// Reaching a final value before it is ever read is what makes this trivially safe rather than an
/// ordering argument: both `register` calls run from ACPI enumeration, which completes before
/// `boot_all_secondaries` starts a single secondary -- so the machine is still single-threaded
/// when this settles, and no shootdown, fault or scheduler walk has happened yet. The Release
/// below and the Acquire in the reader cover the boot path itself, where the BSP registers
/// processors it will later walk.
static MAX_REGISTERED_ID: AtomicUsize = AtomicUsize::new(0);

static CPU_MAIN_BARRIER: AtomicBool = AtomicBool::new(false);

pub fn secondary_entry(id: u32, tcb_base: VirtAddr, kernel_stack_base: *mut u8) -> ! {
    crate::arch::processor::init(tcb_base);
    secondary_main(id, kernel_stack_base)
}

/// See [`init_cpu_locals`].
#[inline(never)]
fn secondary_main(id: u32, kernel_stack_base: *mut u8) -> ! {
    unsafe {
        *BOOT_KERNEL_STACK.borrow_mut() = kernel_stack_base;
        *CPU_ID.borrow_mut() = id;
        *CURRENT_PROCESSOR.get() = &**ALL_PROCESSORS[id as usize].as_ref().unwrap();
    }
    arch::init_secondary();
    crate::pmc::init_cpu();
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
    /* TODO: guard page support */
    // Never freed: this cpu runs on it until the machine goes down.
    let kernel_stack = crate::thread::kstack::leak_one();

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
        attach_caches(&mut cpu_topo_root, topo_path, 0);
        let mut level = &mut *cpu_topo_root;
        for (depth, step) in topo_path.steps.iter().enumerate() {
            // A placeholder `add_child` padded in has no cpus; the first real occupant names it.
            if level
                .child_mut(step.index)
                .is_none_or(|child| child.count() == 0)
            {
                level.add_child(step.index, CPUTopoNode::new(step.kind));
            }
            let child = level.child_mut(step.index).unwrap();
            child.set_cpu(p.id);
            attach_caches(child, topo_path, depth + 1);

            let next = level.child_mut(step.index);
            level = next.unwrap();
        }
    }
    log_topology(&cpu_topo_root, 0);
    crate::processor::sched::set_cpu_topology(cpu_topo_root);
    // Every cpu waited for above has run `arch::processor::init`, so no cpu can be executing
    // without a thread pointer from here on. Lets `tls_ready` stop reading an MSR per call.
    crate::processor::note_all_tls_ready();
    CPU_MAIN_BARRIER.store(true, core::sync::atomic::Ordering::SeqCst);
    crate::memory::prep_smp();
}

/// The first cpu to reach a node describes the caches shared there; every cpu under it reports
/// the same hardware.
fn attach_caches(node: &mut CPUTopoNode, path: &TopoPath, depth: usize) {
    if !node.caches().is_empty() {
        return;
    }
    for (cache_depth, cache) in &path.caches {
        if *cache_depth == depth {
            node.add_cache(*cache);
        }
    }
}

fn log_topology(node: &CPUTopoNode, depth: usize) {
    if node.count() == 0 {
        return;
    }
    log::debug!(
        "topology: {:width$}{:?} #{} ({} cpus){}",
        "",
        node.kind(),
        node.id(),
        node.count(),
        node.caches()
            .iter()
            .fold(alloc::string::String::new(), |mut s, (id, c)| {
                use core::fmt::Write;
                let _ = write!(s, " L{}{:?}#{}:{}K", c.level, c.kind, id, c.size / 1024);
                s
            }),
        width = depth * 2,
    );
    for child in node.children() {
        log_topology(child, depth + 1);
    }
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
    // After the store, and Release, so an Acquire reader that sees this id also sees the slot.
    // Bounded by the check above, so the reader's slice index cannot go out of range.
    MAX_REGISTERED_ID.fetch_max(id as usize, Ordering::Release);
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
    unsafe { ALL_PROCESSORS[id as usize].as_mut().unwrap() }
}

/// Run `f` on every processor that is up.
///
/// Walks only as far as the highest id ever registered, not the whole `MAX_CPU_ID + 1` array.
/// That matters because this is on the TLB shootdown path -- [`ArchTlbMgr::finish_send`] calls it
/// three times per send and [`PendingShootdown::do_wait`] a fourth, at ~14 700 object- and
/// arch-origin sends per boot -- and on the page-fault path. Sweeping 1025 slots to find the four
/// processors a machine has costs the same whether it has four or a thousand, and it is the
/// dominant term in a send that targets nobody, which is 83% of them.
///
/// See [MAX_REGISTERED_ID] for why the bound is final before anything reads it.
/// True when this is a single-processor system, i.e. no processor other than the caller's can
/// exist. Used to skip cross-cpu coordination (TLB-shootdown revoke/target/IPI) that provably has
/// no work on one cpu. `MAX_REGISTERED_ID == 0` means only cpu 0 ever registered.
pub fn is_single_processor() -> bool {
    MAX_REGISTERED_ID.load(Ordering::Acquire) == 0
}

pub fn with_each_active_processor(mut f: impl FnMut(&'static Processor)) {
    let max = MAX_REGISTERED_ID.load(Ordering::Acquire);
    for p in &all_processors()[..=max] {
        if let Some(p) = p {
            if p.is_running() {
                f(p)
            }
        }
    }
}
