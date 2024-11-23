//#![no_std]
#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(duration_constants)]
#![allow(unreachable_code)]
//#![no_main]

/*
#[no_mangle]
pub extern "C" fn std_runtime_starta() {
    twizzler_abi::syscall::sys_kernel_console_write(
        b"hello world\n",
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
    loop {}
}
*/

/*
#[panic_handler]
pub fn __panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
*/

#[thread_local]
static mut FOO: u32 = 42;
#[thread_local]
static mut BAR: u32 = 0;
#[allow(named_asm_labels)]

static BAZ: AtomicU64 = AtomicU64::new(0);

fn test_thread_sync() {
    let _j = std::thread::spawn(|| {
        let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
        let value = 0;
        let wait = ThreadSync::new_sleep(ThreadSyncSleep::new(
            reference,
            value,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));

        loop {
            println!("{:?} going to sleep", std::thread::current().id());
            let res = sys_thread_sync(&mut [wait], None);
            println!("woke up: {:?} {:?}", res, wait.get_result());
        }
    });

    let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
    let wake = ThreadSync::new_wake(ThreadSyncWake::new(reference, 1));
    let mut c = 0u64;
    loop {
        println!("{:?} waking up {}", std::thread::current().id(), c);
        c += 1;
        let res = sys_thread_sync(&mut [wake], None);
        for _i in 0u64..40000u64 {}
        println!("done {:?}", res);
    }
}

fn test_thread_sync_timeout() {
    let _j = std::thread::spawn(|| {
        let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
        let value = 0;
        let wait = ThreadSync::new_sleep(ThreadSyncSleep::new(
            reference,
            value,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));

        let mut c = 0u64;
        loop {
            println!("{:?} going to sleep {}", std::thread::current().id(), c);
            let res = sys_thread_sync(&mut [wait], Some(Duration::MILLISECOND * 1000));
            println!("woke up: {:?} {:?}", res, wait.get_result());
            c += 1;
        }
    });

    let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
    let _wake = ThreadSync::new_wake(ThreadSyncWake::new(reference, 1));
    let mut _c = 0u64;
    loop {

        // println!("{:?} waking up {}", std::thread::current().id(), c);
        // c += 1;
        // let res = sys_thread_sync(&mut [wake], None);
        // for i in 0u64..40000u64 {}
        // println!("done {:?}", res);
    }
}

struct Foo {
    x: u64,
}

fn test_mutex() {
    let mutex: Arc<Mutex<Foo>> = Arc::new(Mutex::new(Foo { x: 0 }));
    let mutex2 = mutex.clone();
    std::thread::spawn(move || {
        let mut c = 0u64;
        loop {
            let mut data = mutex.lock().unwrap();
            data.x += 1;
            let v = data.x;
            c += 1;
            if c % 1000000 == 0 {
                println!("w {}", data.x);
            }
            assert_eq!(v, data.x);
        }
    });

    let mut c = 0u64;
    loop {
        let mut data = mutex2.lock().unwrap();
        data.x += 1;
        c += 1;
        let v = data.x;
        // for i in 0..1000 {}
        assert_eq!(v, data.x);
        if c % 1000000 == 0 {
            println!("a {}", data.x);
        }
        assert_eq!(v, data.x);
    }
}

fn get_user_input() {
    println!("enter some text:");
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .expect("Did not enter a correct string");
    println!("you typed: {}", s);
}

fn list_subobjs(level: usize, id: ObjID) {
    let mut n = 0;
    loop {
        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::GetSubObject(
                SubObjectType::Info.into(),
                n,
            )),
            Some(id),
            0,
            0,
            KactionFlags::empty(),
        );
        if res.is_err() {
            break;
        } else if let KactionValue::ObjID(id) = res.unwrap() {
            println!("  sub {:indent$}info {}: {}", "", n, id, indent = level);
        }
        n += 1;
    }

    let mut n = 0;
    loop {
        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::GetSubObject(
                SubObjectType::Mmio.into(),
                n,
            )),
            Some(id),
            0,
            0,
            KactionFlags::empty(),
        );
        if res.is_err() {
            break;
        } else if let KactionValue::ObjID(id) = res.unwrap() {
            println!("  sub {:indent$}mmio {}: {}", "", n, id, indent = level);
        }
        n += 1;
    }
}

fn enumerate_children(level: usize, id: ObjID) {
    let mut n = 0;
    loop {
        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::GetChild(n)),
            Some(id),
            0,
            0,
            KactionFlags::empty(),
        );
        if res.is_err() {
            break;
        } else if let KactionValue::ObjID(id) = res.unwrap() {
            println!("{:indent$}{}: {}", "", n, id, indent = level);
            list_subobjs(level, id);
            enumerate_children(level + 4, id);
        }
        n = n + 1;
    }
}

fn test_kaction() {
    let res = sys_kaction(
        KactionCmd::Generic(KactionGenericCmd::GetKsoRoot),
        None,
        0,
        0,
        KactionFlags::empty(),
    );
    println!("{:?}", res);
    let id = match res.unwrap() {
        KactionValue::U64(_) => todo!(),
        KactionValue::ObjID(id) => id,
    };

    enumerate_children(0, id);
}

fn exec(name: &str, id: ObjID, argid: ObjID) {
    let env: Vec<String> = std::env::vars()
        .map(|(n, v)| format!("{}={}", n, v))
        .collect();
    let env_ref: Vec<&[u8]> = env.iter().map(|x| x.as_str().as_bytes()).collect();
    let mut args = vec![name.as_bytes()];
    let argstr = format!("{}", argid.as_u128());
    args.push(argstr.as_bytes());
    let _elf = twizzler_abi::runtime::load_elf::spawn_new_executable(id, &args, &env_ref);
    //println!("ELF: {:?}", elf);
}

fn exec2(name: &str, id: ObjID) -> Option<ObjID> {
    let env: Vec<String> = std::env::vars()
        .map(|(n, v)| format!("{}={}", n, v))
        .collect();
    let env_ref: Vec<&[u8]> = env.iter().map(|x| x.as_str().as_bytes()).collect();
    let args = vec![name.as_bytes()];
    twizzler_abi::runtime::load_elf::spawn_new_executable(id, &args, &env_ref).ok()
    //println!("ELF: {:?}", elf);
}

fn exec_n(name: &str, id: ObjID, args: &[&str]) -> Option<ObjID> {
    let env: Vec<String> = std::env::vars()
        .map(|(n, v)| format!("{}={}", n, v))
        .collect();
    let env_ref: Vec<&[u8]> = env.iter().map(|x| x.as_str().as_bytes()).collect();
    let mut fullargs = vec![name.as_bytes()];
    fullargs.extend(args.iter().map(|x| x.as_bytes()));
    twizzler_abi::runtime::load_elf::spawn_new_executable(id, &fullargs, &env_ref).ok()
}

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = twizzler_abi::runtime::get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

fn initialize_pager() { 
    println!("starting pager");
    const DEFAULT_PAGER_QUEUE_LEN: usize = 1024;
    //Create Pager -> Kernel Queue
    let pager_to_kernel_queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
    )
    .unwrap();

    sys_new_handle(
        pager_to_kernel_queue.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();

    //Create Kernel -> Pager Queue
    let kernel_to_pager_queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
    )
    .unwrap();
    sys_new_handle(
        kernel_to_pager_queue.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();
    //Start Pager
    if let Some(id) = find_init_name("pager") {
        exec_n(
            "pager",
            id,
            &[
                &pager_to_kernel_queue.object().id().as_u128().to_string(),
                &kernel_to_pager_queue.object().id().as_u128().to_string(),
            ],
        );
    } else {
        eprintln!("[init] failed to start pager");
    }
}

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .finish(),
    )
    .unwrap();

    // Load and wait for tests to complete
    let lbcomp: CompartmentHandle = CompartmentLoader::new(
        "logboi",
        "liblogboi_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["logboi"])
    .load()
    .unwrap();
    let mut flags = lbcomp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = lbcomp.wait(flags);
    }
    tracing::info!("logboi ready");
    std::mem::forget(lbcomp);

    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    let devid = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();
    info!("starting device manager");
    let dev_comp = monitor_api::CompartmentLoader::new(
        "devmgr",
        "devmgr",
        monitor_api::NewCompartmentFlags::EXPORT_GATES,
    )
    .args(["devmgr", &devid.raw().to_string()])
    .load()
    .expect("failed to start device manager");

    debug!("waiting for device manager to come up");
    let obj = Object::<std::sync::atomic::AtomicU64>::init_id(
        devid,
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let base = unsafe { obj.base_unchecked() };
    twizzler_abi::syscall::sys_thread_sync(
        &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(base),
            0,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ))],
        None,
    )
    .unwrap();
    debug!("device manager is up!");

    initialize_pager();

    std::mem::forget(dev_comp);
    std::mem::forget(pager_comp);

    run_tests("test_bins", false);
    run_tests("bench_bins", true);

    println!("Hi, welcome to the basic twizzler test console.");
    println!("If you wanted line-editing, you've come to the wrong place.");
    println!("To run a program, type its name.");
    loop {
        let reply = rprompt::prompt_reply_stdout("> ").unwrap();
        let cmd: Vec<&str> = reply.split_whitespace().collect();
        if cmd.len() == 0 {
            continue;
        }
        let comp = CompartmentLoader::new(cmd[0], cmd[0], NewCompartmentFlags::empty())
            .args(&cmd)
            .load();
        if let Ok(comp) = comp {
            let mut flags = comp.info().flags;
            while !flags.contains(CompartmentFlags::EXITED) {
                flags = comp.wait(flags);
            }
        } else {
            warn!("failed to start {}", cmd[0]);
        }
    }
}

fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

fn run_tests(test_list_name: &str, benches: bool) {
    if let Some(id) = find_init_name(test_list_name) {
        println!("=== found init test list ===");
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, Protections::READ.into()).unwrap();

        let addr = unsafe { handle.start().add(NULLPAGE_SIZE) };
        let bytes = unsafe {
            core::slice::from_raw_parts(addr as *const u8, twizzler_abi::object::MAX_SIZE)
        };
        let bytes = &bytes[0..bytes.iter().position(|r| *r == 0).unwrap_or(0)];
        let str = String::from_utf8(bytes.to_vec()).unwrap();
        let mut test_failed = false;
        for line in str.split("\n").filter(|l| !l.is_empty()) {
            println!("STARTING TEST {}", line);
            let test_comp = monitor_api::CompartmentLoader::new(
                line,
                line,
                monitor_api::NewCompartmentFlags::empty(),
            )
            .args(&[line, if benches { "--bench" } else { "--test" }])
            .load()
            .expect("failed to load specified test");
            let mut flags = test_comp.info().flags;
            while !flags.contains(monitor_api::CompartmentFlags::EXITED) {
                flags = test_comp.wait(flags);
            }
        }
        // TODO: get exit status, and set this
        if test_failed {
            println!("!!! TEST MODE FAILED");
        }
        #[allow(deprecated)]
        twizzler_abi::syscall::sys_debug_shutdown(if test_failed { 1 } else { 0 });
    }
}

/*
#[naked]
#[no_mangle]
extern "C" fn _start() -> ! {
    unsafe { asm!("call std_runtime_start", options(noreturn)) }
}
*/

use std::{
    sync::{atomic::AtomicU64, Arc, Mutex},
    time::Duration,
};

use monitor_api::{CompartmentFlags, CompartmentHandle, CompartmentLoader, NewCompartmentFlags};
use tracing::{debug, info, warn};
use twizzler_abi::{
    aux::KernelInitInfo,
    device::SubObjectType,
    kso::{KactionCmd, KactionFlags, KactionGenericCmd, KactionValue},
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    pager::{CompletionToKernel, RequestFromKernel},
    syscall::{
        sys_kaction,
        sys_new_handle,
        sys_thread_sync,
        BackingType,
        LifetimeType, //MapFlags,
        NewHandleFlags,
        ObjectCreate,
        ObjectCreateFlags,
        ThreadSync,
        ThreadSyncFlags,
        ThreadSyncOp,
        ThreadSyncReference,
        ThreadSyncSleep,
        ThreadSyncWake,
    },
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_object::{CreateSpec, Object, ObjectInitFlags};
