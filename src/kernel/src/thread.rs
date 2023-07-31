use core::{
    alloc::Layout,
    cell::RefCell,
    sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering},
};

use alloc::{boxed::Box, sync::Arc};
use intrusive_collections::linked_list::AtomicLink;
use twizzler_abi::{
    aux::{AuxEntry, KernelInitInfo, KernelInitName},
    object::{ObjID, Protections},
    syscall::{ThreadSpawnArgs, ThreadSpawnError},
    upcall::UpcallInfo,
};
use xmas_elf::program::SegmentData;

use crate::{
    idcounter::{Id, IdCounter},
    initrd::get_boot_objects,
    interrupt,
    memory::{
        context::{Context, ContextRef, UserContext},
        VirtAddr,
    },
    obj::ObjectRef,
    processor::{get_processor, KERNEL_STACK_SIZE},
    sched::schedule_new_thread,
    spinlock::Spinlock,
    syscall::object::get_vmcontext_from_handle,
};

#[derive(Clone, Copy, PartialEq, Default, Debug)]
#[repr(u32)]
enum PriorityClass {
    RealTime = 0,
    User = 1,
    Background = 2,
    #[default]
    Idle = 3,
    ClassCount = 4,
}

#[derive(Default, Debug)]
pub struct Priority {
    class: PriorityClass,
    adjust: AtomicI32,
}

#[derive(PartialEq, Copy, Clone, Debug)]
#[repr(u32)]
pub enum ThreadState {
    Starting,
    Running,
    Blocked,
    Exiting,
    Exited,
}

#[derive(Debug, Default)]
pub struct ThreadStats {
    pub user: AtomicU64,
    pub sys: AtomicU64,
    pub idle: AtomicU64,
    pub last: AtomicU64,
}
const THREAD_PROC_IDLE: u32 = 1;
const THREAD_HAS_DONATED_PRIORITY: u32 = 2;
const THREAD_IN_KERNEL: u32 = 4;
const THREAD_IS_SYNC_SLEEP: u32 = 8;
const THREAD_IS_SYNC_SLEEP_DONE: u32 = 16;
pub struct Thread {
    pub arch: crate::arch::thread::ArchThread,
    pub priority: Priority,
    pub flags: AtomicU32,
    pub last_cpu: AtomicI32,
    pub affinity: AtomicI32,
    pub state: AtomicU32,
    pub critical_counter: AtomicU64,
    id: Id<'static>,
    pub switch_lock: AtomicU64,
    pub donated_priority: Spinlock<Option<Priority>>,
    pub current_processor_queue: AtomicI32,
    memory_context: Option<ContextRef>,
    pub kernel_stack: Box<[u8; KERNEL_STACK_SIZE]>,
    pub stats: ThreadStats,
    spawn_args: Option<ThreadSpawnArgs>,
    pub repr: Option<ObjectRef>,
    pub sched_link: AtomicLink,
}
unsafe impl Send for Thread {}

pub type ThreadRef = Arc<Thread>;

#[thread_local]
static CURRENT_THREAD: RefCell<Option<ThreadRef>> = RefCell::new(None);

pub fn current_thread_ref() -> Option<ThreadRef> {
    if core::intrinsics::unlikely(!crate::processor::tls_ready()) {
        return None;
    }
    interrupt::with_disabled(|| CURRENT_THREAD.borrow().clone())
}

pub fn set_current_thread(thread: ThreadRef) {
    interrupt::with_disabled(move || {
        let old = CURRENT_THREAD.replace(Some(thread));
        drop(old);
    });
}

pub fn enter_kernel() {
    if let Some(thread) = current_thread_ref() {
        thread.flags.fetch_or(THREAD_IN_KERNEL, Ordering::SeqCst);
    }
}

pub fn exit_kernel() {
    if let Some(thread) = current_thread_ref() {
        thread.flags.fetch_and(!THREAD_IN_KERNEL, Ordering::SeqCst);
    }
}

static ID_COUNTER: IdCounter = IdCounter::new();

pub fn current_memory_context() -> Option<ContextRef> {
    current_thread_ref()
        .map(|t| t.memory_context.clone())
        .flatten()
}

impl Thread {
    pub fn new() -> Self {
        /* TODO: dedicated kernel stack allocator, with guard page support */
        let kernel_stack = unsafe {
            let layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
            alloc::alloc::alloc_zeroed(layout)
        };
        Self {
            arch: crate::arch::thread::ArchThread::new(),
            priority: Priority {
                class: PriorityClass::User,
                adjust: AtomicI32::new(0),
            },
            id: ID_COUNTER.next(),
            flags: AtomicU32::new(THREAD_IN_KERNEL),
            state: AtomicU32::new(ThreadState::Starting as u32),
            kernel_stack: unsafe { Box::from_raw(core::intrinsics::transmute(kernel_stack)) },
            critical_counter: AtomicU64::new(0),
            switch_lock: AtomicU64::new(0),
            affinity: AtomicI32::new(-1),
            last_cpu: AtomicI32::new(-1),
            donated_priority: Spinlock::new(None),
            current_processor_queue: AtomicI32::new(-1),
            stats: ThreadStats::default(),
            memory_context: None,
            spawn_args: None,
            repr: None,
            sched_link: AtomicLink::default(),
        }
    }

    // TODO: cleanup all these new variants
    pub fn new_with_new_vm() -> Self {
        let mut thread = Self::new();
        thread.memory_context = Some(Arc::new(Context::new()));
        thread
    }

    pub fn new_with_current_context(spawn_args: ThreadSpawnArgs) -> Self {
        let mut thread = Self::new();
        thread.memory_context = Some(current_memory_context().unwrap());
        thread.spawn_args = Some(spawn_args);
        thread
    }

    pub fn new_with_handle_context(spawn_args: ThreadSpawnArgs, vmc: ContextRef) -> Self {
        let mut thread = Self::new();
        thread.memory_context = Some(vmc);
        thread.spawn_args = Some(spawn_args);
        thread
    }

    pub fn new_idle() -> Self {
        let mut thread = Self::new();
        thread.flags.fetch_or(THREAD_PROC_IDLE, Ordering::SeqCst);
        thread.priority.class = PriorityClass::Idle;
        thread.switch_lock.store(1, Ordering::SeqCst);
        thread
    }

    pub fn set_sync_sleep(&self) {
        self.flags.fetch_or(THREAD_IS_SYNC_SLEEP, Ordering::SeqCst);
    }

    pub fn reset_sync_sleep(&self) -> bool {
        let old = self
            .flags
            .fetch_and(!THREAD_IS_SYNC_SLEEP, Ordering::SeqCst);
        (old & THREAD_IS_SYNC_SLEEP) != 0
    }

    pub fn set_sync_sleep_done(&self) {
        self.flags
            .fetch_or(THREAD_IS_SYNC_SLEEP_DONE, Ordering::SeqCst);
    }

    pub fn reset_sync_sleep_done(&self) -> bool {
        let old = self
            .flags
            .fetch_and(!THREAD_IS_SYNC_SLEEP_DONE, Ordering::SeqCst);
        (old & THREAD_IS_SYNC_SLEEP_DONE) != 0
    }

    pub fn switch_thread(&self, current: &Thread) {
        if self != current {
            if let Some(ref ctx) = self.memory_context {
                ctx.switch_to();
            }
        }
        self.arch_switch_to(current)
    }

    pub fn do_critical<F, T>(&self, mut f: F) -> T
    where
        F: FnMut(&Self) -> T,
    {
        self.critical_counter.fetch_add(1, Ordering::SeqCst);
        let res = f(self);
        self.critical_counter.fetch_sub(1, Ordering::SeqCst);
        res
    }

    #[inline]
    pub fn is_critical(&self) -> bool {
        self.critical_counter.load(Ordering::SeqCst) > 0
    }

    #[inline]
    pub fn exit_critical(&self) {
        let res = self.critical_counter.fetch_sub(1, Ordering::SeqCst);
        assert!(res > 0);
    }

    #[inline]
    pub fn enter_critical(&self) -> CriticalGuard {
        self.critical_counter.fetch_add(1, Ordering::SeqCst);
        CriticalGuard { thread: self }
    }

    #[inline]
    pub fn enter_critical_unguarded(&self) {
        self.critical_counter.fetch_add(1, Ordering::SeqCst);
    }

    #[inline]
    pub fn queue_number<const NR_QUEUES: usize>(&self) -> usize {
        self.priority.queue_number::<NR_QUEUES>()
    }

    #[inline]
    pub fn is_idle_thread(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_PROC_IDLE != 0
    }

    #[inline]
    pub fn is_in_user(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_IN_KERNEL == 0
    }

    pub fn effective_priority(&self) -> Priority {
        if self.flags.load(Ordering::SeqCst) & THREAD_HAS_DONATED_PRIORITY != 0 {
            let donated_priority = self.donated_priority.lock();
            if let Some(ref donated) = *donated_priority {
                return core::cmp::max(donated.clone(), self.priority.clone());
            }
        }
        self.priority.clone()
    }

    pub fn donate_priority(&self, pri: Priority) -> bool {
        let current_priority = self.effective_priority();
        let mut donated_priority = self.donated_priority.lock();
        if let Some(ref current) = *donated_priority {
            if current > &pri {
                return false;
            }
        }
        let needs_resched = pri > current_priority;
        *donated_priority = Some(pri);
        self.flags
            .fetch_or(THREAD_HAS_DONATED_PRIORITY, Ordering::SeqCst);
        if needs_resched {
            if let Some(cur) = current_thread_ref() {
                if cur.id() == self.id() {
                    return true;
                }
            }
            self.maybe_reschedule_thread();
        }
        true
    }

    pub fn maybe_reschedule_thread(&self) {
        let ccpu = self.current_processor_queue.load(Ordering::SeqCst);
        /* if we get -1 here, the thread is either running or blocked, not waiting on a queue. There's a small race condition, here, though,
        since we check this variable and then lock a scheduler queue. It's possible that the thread was placed on a queue, then this variable was set,
        and then we load it, and then the thread is run. This results in a spurious reschedule. It's probably rare, though, but we should profile this
        to see if it's a problem.

        Another possible race condition is the opposite: a thread is running, and we read -1, and then it gets put on the queue. This is also probably
        okay, since that means that we might not have really needed to do a reschedule.

        Finally, note that this function should be called with the donated_priority lock held, since that will force serialization by any schedulers
        calculating the thread's priority at the time of this call. Or, if the HAS_DONATED_PRIORITY flag is clear, it will not, but that is okay too.
        But this does mean we need to submit any wakeups/reschedules with interrupts cleared. */
        //TODO: verify the above logic
        //TODO: optimize this by keeping an is_running flag?
        if ccpu == -1 {
            return;
        }
        let ccpu = ccpu as u32;
        let proc = get_processor(ccpu);
        let resched = proc.schedlock().check_priority_change(self);
        if resched {
            interrupt::with_disabled(|| proc.wakeup(true));
        }
    }

    pub fn remove_donated_priority(&self) {
        let current_priority = self.effective_priority();
        let mut donated_priority = self.donated_priority.lock();
        self.flags
            .fetch_and(!THREAD_HAS_DONATED_PRIORITY, Ordering::SeqCst);
        *donated_priority = None;
        if current_priority < self.effective_priority() {
            self.maybe_reschedule_thread();
        }
    }

    pub fn get_donated_priority(&self) -> Option<Priority> {
        let d = self.donated_priority.lock();
        (*d).clone()
    }

    #[inline]
    pub fn set_state(&self, state: ThreadState) {
        self.state.store(state as u32, Ordering::SeqCst);
    }

    #[inline]
    pub fn state(&self) -> ThreadState {
        unsafe { core::intrinsics::transmute(self.state.load(Ordering::SeqCst)) }
    }

    #[inline]
    pub fn id(&self) -> u64 {
        self.id.value()
    }

    pub fn send_upcall(&self, info: UpcallInfo) {
        // TODO
        let ctx = current_memory_context().unwrap();
        let upcall = ctx.get_upcall().unwrap();
        self.arch_queue_upcall(upcall, info);
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        //logln!("drop thread {}", self.id());
    }
}

impl Eq for Thread {}

impl PartialEq for Thread {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl PartialOrd for Thread {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for Thread {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

pub struct CriticalGuard<'a> {
    thread: &'a Thread,
}

impl<'a> Drop for CriticalGuard<'a> {
    fn drop(&mut self) {
        self.thread.exit_critical();
    }
}

impl Priority {
    #[allow(clippy::declare_interior_mutable_const)]
    pub const REALTIME: Self = Self {
        class: PriorityClass::RealTime,
        adjust: AtomicI32::new(0),
    };
    pub fn queue_number<const NR_QUEUES: usize>(&self) -> usize {
        assert_eq!(NR_QUEUES % PriorityClass::ClassCount as usize, 0);
        let queues_per_class = NR_QUEUES / PriorityClass::ClassCount as usize;
        assert!(queues_per_class > 0 && queues_per_class % 2 == 0);
        let equilibrium = (queues_per_class / 2) as i32;
        let base_queue = self.class as usize * queues_per_class + equilibrium as usize;
        let adj = self
            .adjust
            .load(Ordering::SeqCst)
            .clamp(-equilibrium, equilibrium);
        let q = ((base_queue as i32) + adj) as usize;
        assert!(q < NR_QUEUES);
        q
    }

    pub fn from_queue_number<const NR_QUEUES: usize>(queue: usize) -> Self {
        if queue == NR_QUEUES {
            return Self {
                class: PriorityClass::Idle,
                adjust: AtomicI32::new(i32::MAX),
            };
        }
        let queues_per_class = NR_QUEUES / PriorityClass::ClassCount as usize;
        let class = queue / queues_per_class;
        assert!(class < PriorityClass::ClassCount as usize);
        let equilibrium = queues_per_class / 2;
        let base_queue = class * queues_per_class + equilibrium;
        let adj = queue as i32 - base_queue as i32;
        Self {
            class: unsafe { core::intrinsics::transmute(class as u32) },
            adjust: AtomicI32::new(adj),
        }
    }
}

impl PartialEq for Priority {
    fn eq(&self, other: &Self) -> bool {
        self.class == other.class
            && self.adjust.load(Ordering::Relaxed) == other.adjust.load(Ordering::Relaxed)
    }
}

impl PartialOrd for PriorityClass {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        /* backwards because of how priority works */
        (*other as usize).partial_cmp(&(*self as usize))
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        match self.class.partial_cmp(&other.class) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        let thisadj = self.adjust.load(Ordering::Relaxed);
        let thatadj = other.adjust.load(Ordering::Relaxed);
        /* backwards because of how priority works */
        thatadj.partial_cmp(&thisadj)
    }
}

impl Clone for Priority {
    fn clone(&self) -> Self {
        Self {
            class: self.class,
            adjust: AtomicI32::new(self.adjust.load(Ordering::SeqCst)),
        }
    }
}

impl Eq for Priority {
    fn assert_receiver_is_total_eq(&self) {}
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        //is this okay?
        self.partial_cmp(other).unwrap()
    }
}

pub fn exit() {
    {
        let th = current_thread_ref().unwrap();
        crate::interrupt::disable();
        th.set_state(ThreadState::Exiting);
        crate::syscall::sync::remove_from_requeue(&th);
        crate::sched::remove_thread(th.id());
        drop(th);
    }
    crate::sched::schedule(false);
}

/*
fn object_copy_test() {
    let obj1 = crate::obj::Object::new();
    let obj2 = crate::obj::Object::new();

    let page = crate::obj::pages::Page::new();
    let slice = page.as_mut_slice();
    slice[0] = 9;
    obj1.add_page(1.into(), crate::obj::pages::Page::new());

    let page = crate::obj::pages::Page::new();
    let slice = page.as_mut_slice();
    slice[0] = 10;
    obj1.add_page(2.into(), crate::obj::pages::Page::new());

    let page = crate::obj::pages::Page::new();
    let slice = page.as_mut_slice();
    slice[0] = 11;
    obj1.add_page(3.into(), crate::obj::pages::Page::new());

    let page = crate::obj::pages::Page::new();
    let slice = page.as_mut_slice();
    slice[0] = 12;
    obj1.add_page(5.into(), crate::obj::pages::Page::new());

    let page = crate::obj::pages::Page::new();
    let slice = page.as_mut_slice();
    slice[0] = 13;
    obj1.add_page(6.into(), crate::obj::pages::Page::new());

    let obj1 = Arc::new(obj1);
    let obj2 = Arc::new(obj2);

    crate::obj::copy::copy_ranges(&obj1, 1.into(), &obj2, 8.into(), 4);

    obj1.print_page_tree();
    obj2.print_page_tree();

    logln!("====== TEST FAULT ======\n");
    let res = obj2.lock_page_tree().get_page(10.into(), true);
    logln!("fault => {:?}", res);
    obj1.print_page_tree();
    obj2.print_page_tree();
}
*/

fn create_blank_object() -> ObjectRef {
    let obj = crate::obj::Object::new();
    let obj = Arc::new(obj);
    crate::obj::register_object(obj.clone());
    obj
}

fn create_name_object() -> ObjectRef {
    let boot_objects = get_boot_objects();
    let obj = create_blank_object();
    let mut init_info = KernelInitInfo::new();
    for (name, obj) in &boot_objects.name_map {
        init_info.add_name(KernelInitName::new(name, obj.id()));
    }
    obj.write_base(&init_info);
    obj
}

extern "C" fn user_init() {
    /* We need this scope to drop everything before we jump to user */
    let (aux_start, entry) = {
        let vm = current_memory_context().unwrap();
        let boot_objects = get_boot_objects();

        let obj_text = create_blank_object();
        let obj_data = create_blank_object();
        let obj_stack = create_blank_object();
        let obj_name = create_name_object();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_TEXT,
            obj_text.clone(),
            vm.clone(),
            Protections::READ | Protections::EXEC | Protections::WRITE,
        )
        .unwrap();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_DATA,
            obj_data,
            vm.clone(),
            Protections::READ | Protections::WRITE,
        )
        .unwrap();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_STACK,
            obj_stack,
            vm.clone(),
            Protections::READ | Protections::WRITE,
        )
        .unwrap();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_KERNEL_INIT,
            obj_name,
            vm.clone(),
            Protections::READ,
        )
        .unwrap();
        let init_obj = boot_objects.init.as_ref().expect("no init found");
        let obj1_data = crate::operations::read_object(init_obj);
        let elf = xmas_elf::ElfFile::new(&obj1_data).unwrap();
        let mut phinfo = None;
        for ph in elf.program_iter() {
            if ph.get_type() == Ok(xmas_elf::program::Type::Load) {
                let file_data = ph.get_data(&elf).unwrap();
                if let SegmentData::Undefined(file_data) = file_data {
                    let memory_addr = VirtAddr::new(ph.virtual_addr()).unwrap();
                    let memory_slice: &mut [u8] = unsafe {
                        core::slice::from_raw_parts_mut(
                            memory_addr.as_mut_ptr(),
                            ph.mem_size() as usize,
                        )
                    };

                    memory_slice.fill(0);
                    (&mut memory_slice[0..ph.file_size() as usize]).copy_from_slice(file_data);
                }
            }
            if ph.get_type() == Ok(xmas_elf::program::Type::Phdr) {
                phinfo = Some(ph);
            }
        }

        fn append_aux(aux: *mut AuxEntry, entry: AuxEntry) -> *mut AuxEntry {
            unsafe {
                *aux = entry;
                aux.add(1)
            }
        }

        let aux_start: u64 = (1 << 30) * 2 + 0x300000;
        let aux_start = aux_start as *mut twizzler_abi::aux::AuxEntry;
        let mut aux = aux_start;

        if let Some(phinfo) = phinfo {
            aux = append_aux(
                aux,
                AuxEntry::ProgramHeaders(
                    phinfo.virtual_addr(),
                    phinfo.mem_size() as usize / elf.header.pt2.ph_entry_size() as usize,
                ),
            )
        }

        aux = append_aux(aux, AuxEntry::ExecId(init_obj.id()));
        append_aux(aux, AuxEntry::Null);

        // remove permission mappings from text segment
        let page_tree = obj_text.lock_page_tree();
        for r in page_tree.range(0.into()..usize::MAX.into()) {
            let range = *r.0..r.0.offset(r.1.length);
            vm.invalidate_object(obj_text.id(), &range, crate::obj::InvalidateMode::WriteProtect);
        }

        (aux_start, elf.header.pt2.entry_point())
    };

    unsafe {
        crate::arch::jump_to_user(
            VirtAddr::new(entry).unwrap(),
            VirtAddr::new((1 << 30) * 2 + 0x200000).unwrap(),
            aux_start as u64,
        );
    }
}

extern "C" fn user_new_start() {
    let (entry, stack_base, stack_size, arg) = {
        /* we need this scope to drop the current thread ref before jumping to user */
        let current = current_thread_ref().unwrap();
        let args = current.spawn_args.as_ref().unwrap();
        current.set_tls(args.tls as u64);
        /*
        logln!(
            "thread jtu {:x} {:x} {:x}",
            args.entry,
            args.stack_base + args.stack_size,
            args.tls
        );
        */
        (args.entry, args.stack_base, args.stack_size, args.arg)
    };
    unsafe {
        crate::arch::jump_to_user(
            VirtAddr::new(entry as u64).unwrap(),
            /* TODO: this is x86 specific */
            VirtAddr::new((stack_base + stack_size - 8) as u64).unwrap(),
            arg as u64,
        )
    }
}

pub fn start_new_user(args: ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    let mut thread = if let Some(handle) = args.vm_context_handle {
        let vmc = get_vmcontext_from_handle(handle).ok_or(ThreadSpawnError::NotFound)?;
        Thread::new_with_handle_context(args, vmc)
    } else {
        Thread::new_with_current_context(args)
    };
    unsafe {
        thread.init(user_new_start);
    }
    thread.repr = Some(create_blank_object());
    let id = thread.repr.as_ref().unwrap().id();
    /*
    logln!(
        "starting new thread {} {} with stack k={:p} u={:x},{:x}",
        thread.id,
        id,
        thread.kernel_stack,
        args.stack_base,
        args.stack_size,
    );
    */
    schedule_new_thread(thread);
    Ok(id)
}

pub fn start_new_init() {
    let mut thread = Thread::new_with_new_vm();
    /*
    logln!(
        "starting new thread {} with stack {:p}",
        thread.id,
        thread.kernel_stack
    );
    */
    unsafe {
        thread.init(user_init);
    }
    thread.repr = Some(create_blank_object());
    schedule_new_thread(thread);
}

pub fn start_new_kernel(pri: Priority, start: extern "C" fn()) -> ThreadRef {
    let mut thread = Thread::new();
    thread.priority = pri;
    unsafe { thread.init(start) }
    schedule_new_thread(thread)
}
