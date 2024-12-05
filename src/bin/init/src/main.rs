extern crate twizzler_runtime;
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
    let netid = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();
    let devid = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();
    println!("starting device manager");
    let dev_comp = monitor_api::CompartmentLoader::new(
        "devmgr",
        "devmgr",
        monitor_api::NewCompartmentFlags::EXPORT_GATES,
    )
    .args(["devmgr", &devid.raw().to_string()])
    .load()
    .expect("failed to start device manager");

    println!("waiting for device manager to come up");
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
    println!("device manager is up!");

    println!("starting pager");
    const DEFAULT_PAGER_QUEUE_LEN: usize = 1024;
    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
    )
    .unwrap();

    sys_new_handle(
        queue.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();
    let queue2 = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
    )
    .unwrap();
    sys_new_handle(
        queue2.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();
    let pager_comp = monitor_api::CompartmentLoader::new(
        "pager",
        "pager",
        monitor_api::NewCompartmentFlags::EXPORT_GATES,
    )
    .args([
        "pager",
        &queue.object().id().as_u128().to_string(),
        &queue2.object().id().as_u128().to_string(),
    ])
    .load()
    .expect("failed to start pager");

    std::mem::forget(dev_comp);
    std::mem::forget(pager_comp);

    run_tests("test_bins", false);
    run_tests("bench_bins", true);

    println!("Hi, welcome to the basic twizzler test console.");
    println!("If you wanted line-editing, you've come to the wrong place.");
    println!("A couple commands you can run:");
    println!("   - 'nt': Run the nettest program");
    println!("... and that's it, but you can add your OWN things with the magic of PROGRAMMING.");
    loop {
        let reply = rprompt::prompt_reply_stdout("> ").unwrap();
        println!("got: <{}>", reply);
        /*
        let cmd: Vec<&str> = reply.split(" ").collect();
        if cmd.len() == 2 && cmd[0] == "run" {
            if let Some(id) = find_init_name(cmd[1]) {
                if cmd[1] == "nettest" {
                    exec(cmd[1], id, netid);
                } else {
                    exec(cmd[1], id, ObjID::new(0));
                }
            } else {
                eprintln!("[init] failed to start {}", cmd[1]);
            }
        }

        if cmd.len() == 1 && cmd[0] == "nt" {
            if let Some(id) = find_init_name("nettest") {
                exec("nettest", id, netid);
            } else {
                eprintln!("[init] failed to start nettest");
            }
        }
        */

        //  get_user_input();
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
